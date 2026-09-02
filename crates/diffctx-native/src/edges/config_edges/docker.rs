use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::DOCKER;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, add_edge, link_by_name, link_by_path_match,
};

static DOCKERFILE_COPY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?mi)^(?:COPY|ADD)\s+(?:--[^\s]+\s+)*(.+)").unwrap());
static DOCKERFILE_ENV_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?mi)^ENV\s+(\w+)").unwrap());
static DOCKERFILE_ARG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?mi)^ARG\s+(\w+)").unwrap());

static COMPOSE_BUILD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s+build:\s*['"]?([^'"#\n]+)"##).unwrap());
static COMPOSE_CONTEXT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s+context:\s*['"]?([^'"#\n]+)"##).unwrap());
static COMPOSE_VOLUME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s+-\s*['"]?([./][^:'"\n]+):"#).unwrap());

fn is_dockerfile(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile")
}

fn is_compose_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "docker-compose.yml"
        || name == "docker-compose.yaml"
        || name == "compose.yml"
        || name == "compose.yaml"
}

fn strip_dot_slash(s: &str) -> &str {
    let mut s = s;
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    s
}

fn split_copy_sources(raw: &str) -> Vec<&str> {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.len() < 2 {
        return vec![];
    }
    tokens[..tokens.len() - 1].to_vec()
}

fn collect_dockerfile_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in DOCKERFILE_COPY_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            for src in split_copy_sources(m.as_str()) {
                if !src.starts_with("--") && !src.starts_with('$') {
                    let cleaned =
                        strip_dot_slash(src.trim().trim_matches(|c| c == '\'' || c == '"'));
                    if !cleaned.is_empty() {
                        refs.insert(cleaned.to_string());
                    }
                }
            }
        }
    }

    refs
}

fn collect_compose_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in COMPOSE_BUILD_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            let val = strip_dot_slash(m.as_str().trim());
            if !val.is_empty() {
                refs.insert(val.to_string());
            }
        }
    }

    for cap in COMPOSE_VOLUME_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            let val = strip_dot_slash(m.as_str().trim());
            if !val.is_empty() {
                refs.insert(val.to_string());
            }
        }
    }

    refs
}

pub struct DockerEdgeBuilder;

impl EdgeBuilder for DockerEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let dockerfiles: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_dockerfile(Path::new(f.path())))
            .collect();
        let compose_files: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_compose_file(Path::new(f.path())))
            .collect();

        if dockerfiles.is_empty() && compose_files.is_empty() {
            return EdgeDict::default();
        }

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges = EdgeDict::default();

        for df in &dockerfiles {
            let copy_refs = collect_dockerfile_refs(&df.content);
            for r in &copy_refs {
                link_by_name(
                    &df.id,
                    r,
                    &idx,
                    &mut edges,
                    DOCKER.copy_weight,
                    DOCKER.reverse_factor,
                );
            }

            let has_env = DOCKERFILE_ENV_RE.is_match(&df.content);
            let has_arg = DOCKERFILE_ARG_RE.is_match(&df.content);
            if has_env || has_arg {
                for f in fragments {
                    let fpath = Path::new(f.path());
                    let fname = fpath
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let ext = fpath
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if (ext == "env" || fname.starts_with(".env")) && f.id != df.id {
                        add_edge(
                            &mut edges,
                            &df.id,
                            &f.id,
                            DOCKER.weight,
                            DOCKER.reverse_factor,
                        );
                    }
                }
            }
        }

        for cf in &compose_files {
            let df_dir = Path::new(cf.path()).parent();
            for df in &dockerfiles {
                let dockerfile_dir = Path::new(df.path()).parent();
                if let (Some(cf_parent), Some(df_parent)) = (df_dir, dockerfile_dir) {
                    if df_parent == cf_parent || df_parent.parent() == Some(cf_parent) {
                        add_edge(
                            &mut edges,
                            &cf.id,
                            &df.id,
                            DOCKER.compose_weight,
                            DOCKER.reverse_factor,
                        );
                    }
                }
            }

            let compose_refs = collect_compose_refs(&cf.content);
            for r in &compose_refs {
                link_by_name(
                    &cf.id,
                    r,
                    &idx,
                    &mut edges,
                    DOCKER.compose_weight,
                    DOCKER.reverse_factor,
                );
            }

            for cap in COMPOSE_CONTEXT_RE.captures_iter(&cf.content) {
                if let Some(m) = cap.get(1) {
                    let context = m.as_str().trim();
                    if !context.is_empty() && !context.starts_with('$') {
                        let context_stripped = strip_dot_slash(context).trim_matches('/');
                        // `context: .` — the ubiquitous form — stripped to `.`
                        // and `path.contains(".")` matched every file with an
                        // extension: the compose file linked to the whole
                        // universe at naming weight. `.` and `` mean the
                        // compose file's own directory; a named context is a
                        // path reference and takes the path channel's bar.
                        // In the index's key space (repo-relative), not the
                        // fragment's absolute path. A root compose file's
                        // `context: .` names the whole repository — a region,
                        // not a dependency — and an empty reference abstains.
                        let rel = base::index_key_of(cf.path(), repo_root);
                        let dir = Path::new(&rel)
                            .parent()
                            .map(|d| d.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let target = if context_stripped.is_empty() || context_stripped == "." {
                            dir
                        } else if dir.is_empty() {
                            context_stripped.to_string()
                        } else {
                            format!("{dir}/{context_stripped}")
                        };
                        link_by_path_match(
                            &cf.id,
                            &target,
                            &idx,
                            &mut edges,
                            DOCKER.compose_weight * DOCKER.compose_context_modifier,
                            DOCKER.reverse_factor,
                        );
                    }
                }
            }

            for cap in COMPOSE_VOLUME_RE.captures_iter(&cf.content) {
                if let Some(m) = cap.get(1) {
                    let vol = strip_dot_slash(m.as_str().trim());
                    if !vol.is_empty() {
                        link_by_name(
                            &cf.id,
                            vol,
                            &idx,
                            &mut edges,
                            DOCKER.compose_weight * DOCKER.compose_volume_modifier,
                            DOCKER.reverse_factor,
                        );
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
        let docker_files: Vec<&PathBuf> = changed
            .iter()
            .filter(|p| is_dockerfile(p) || is_compose_file(p))
            .collect();
        if docker_files.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();

        for df in &docker_files {
            let content = match base::read_file_cached(df, file_cache) {
                Some(c) => c,
                None => continue,
            };

            if is_dockerfile(df) {
                refs.extend(collect_dockerfile_refs(&content));
            }
            if is_compose_file(df) {
                refs.extend(collect_compose_refs(&content));
            }
        }

        base::discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FragmentId, FragmentKind};
    use std::sync::Arc;

    fn frag(path: &str, content: &str) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), 1, content.lines().count().max(1) as u32),
            kind: FragmentKind::Chunk,
            content: Arc::from(content),
            identifiers: rustc_hash::FxHashSet::default(),
            token_count: content.len() as u32,
            symbol_name: None,
        }
    }

    /// `context: ./api` from a compose file under `deploy/` names
    /// `deploy/api/`; with absolute fragment paths the reference has to be
    /// built in the index's repo-relative key space or it matches nothing.
    #[test]
    fn a_named_compose_context_links_to_that_directory() {
        let root = "/repo";
        let fragments = vec![
            frag(
                "/repo/deploy/docker-compose.yml",
                "services:\n  api:\n    build:\n      context: ./api\n",
            ),
            frag("/repo/deploy/api/Dockerfile", "FROM python:3.12\n"),
            frag("/repo/deploy/api/app.py", "print(1)\n"),
            frag("/repo/src/other.py", "print(2)\n"),
        ];
        let edges = DockerEdgeBuilder.build(&fragments, Some(Path::new(root)));
        let mut targets: Vec<String> = edges
            .keys()
            .filter(|(src, _)| src.path.as_ref() == "/repo/deploy/docker-compose.yml")
            .map(|(_, dst)| dst.path.to_string())
            .collect();
        targets.sort();
        targets.dedup();
        assert!(
            targets.iter().any(|t| t.starts_with("/repo/deploy/api/")),
            "context: ./api produced no edge into deploy/api: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t == "/repo/src/other.py"),
            "linked outside the context: {targets:?}"
        );
    }
}
