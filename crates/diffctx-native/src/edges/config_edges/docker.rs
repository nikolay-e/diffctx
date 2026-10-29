use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::DOCKER;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, MAX_FILES_PER_PATH_REF, add_edge, link_by_name,
    link_by_path_match,
};

static DOCKERFILE_COPY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?mi)^(?:COPY|ADD)\s+(?:--[^\s]+\s+)*(.+)").unwrap());

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

/// The repo-relative path `reference` names from a file in `base_dir`; `None`
/// for a reference that is not a plain relative path (absolute, a variable, a
/// URL, a wildcard) or that climbs out of the repository.
fn resolve_declared(base_dir: &str, reference: &str) -> Option<String> {
    if reference.starts_with('/')
        || reference.contains(['$', '*', '{', ':'])
        || reference.trim_matches('.').is_empty()
    {
        return None;
    }
    let joined = if base_dir.is_empty() {
        reference.to_string()
    } else {
        format!("{base_dir}/{reference}")
    };
    base::normalize_lexical(&joined).filter(|p| !p.is_empty())
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Links a path a Docker file spells (a `COPY` source, a compose volume or
/// build) to the file or directory it names from the declaring file's own
/// directory. Only when nothing lives there does the reference fall back to
/// the bare-name channel: `./realm.json` in `services/api/` is that file, not
/// every `realm.json` in the repository.
#[allow(clippy::too_many_arguments)]
fn link_declared(
    src: &FragmentId,
    declaring_rel: &str,
    reference: &str,
    idx: &FragmentIndex,
    edges: &mut EdgeDict,
    weight: f64,
    reverse_factor: f64,
) {
    if let Some(resolved) = resolve_declared(dir_of(declaring_rel), reference) {
        let targets = idx.representatives_at(&resolved);
        if !targets.is_empty() {
            if targets.len() <= MAX_FILES_PER_PATH_REF {
                for target in targets {
                    if target != src {
                        add_edge(edges, src, target, weight, reverse_factor);
                    }
                }
            }
            return;
        }
    }
    link_by_name(src, reference, idx, edges, weight, reverse_factor);
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
            let df_rel = base::index_key_of(df.path(), repo_root);
            for r in &copy_refs {
                link_declared(
                    &df.id,
                    &df_rel,
                    r,
                    &idx,
                    &mut edges,
                    DOCKER.copy_weight,
                    DOCKER.reverse_factor,
                );
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

            let cf_rel = base::index_key_of(cf.path(), repo_root);
            let compose_refs = collect_compose_refs(&cf.content);
            for r in &compose_refs {
                link_declared(
                    &cf.id,
                    &cf_rel,
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
                        link_declared(
                            &cf.id,
                            &cf_rel,
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
        let candidate_rels: Vec<(String, &PathBuf)> = candidates
            .iter()
            .map(|c| (base::index_key_of(&c.to_string_lossy(), repo_root), c))
            .collect();
        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut found: FxHashSet<PathBuf> = FxHashSet::default();
        // A reference that names a file or directory from its declaring file
        // brings that and nothing else; only one that names nothing there
        // falls back to the bare name.
        let mut fallback: FxHashSet<String> = FxHashSet::default();
        for path in changed
            .iter()
            .filter(|p| is_dockerfile(p) || is_compose_file(p))
        {
            let Some(content) = base::read_file_cached(path, file_cache) else {
                continue;
            };
            let mut refs = FxHashSet::default();
            if is_dockerfile(path) {
                refs.extend(collect_dockerfile_refs(&content));
            }
            if is_compose_file(path) {
                refs.extend(collect_compose_refs(&content));
            }
            let rel = base::index_key_of(&path.to_string_lossy(), repo_root);
            for r in refs {
                let hits: Vec<&PathBuf> = resolve_declared(dir_of(&rel), &r)
                    .map(|resolved| {
                        let under = format!("{resolved}/");
                        candidate_rels
                            .iter()
                            .filter(|(c, _)| *c == resolved || c.starts_with(&under))
                            .map(|(_, c)| *c)
                            .collect()
                    })
                    .unwrap_or_default();
                if hits.is_empty() {
                    fallback.insert(r);
                } else if hits.len() <= MAX_FILES_PER_PATH_REF {
                    found.extend(hits.into_iter().cloned());
                }
            }
        }
        found.extend(base::discover_files_by_refs(
            &fallback, changed, candidates, repo_root,
        ));
        found.retain(|p| !changed_set.contains(p));
        let mut out: Vec<PathBuf> = found.into_iter().collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;
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

    fn targets_of(edges: &EdgeDict, from: &str) -> Vec<String> {
        let mut t: Vec<String> = edges
            .keys()
            .filter(|(src, _)| src.path.as_ref() == from)
            .map(|(_, dst)| dst.path.to_string())
            .collect();
        t.sort();
        t.dedup();
        t
    }

    /// `./realm.json` in `services/api/` is that file, not every `realm.json`.
    #[test]
    fn a_relative_volume_names_the_file_beside_the_compose_file() {
        let fragments = vec![
            frag(
                "services/api/docker-compose.yml",
                "services:\n  app:\n    volumes:\n      - ./realm.json:/opt/realm.json\n",
            ),
            frag("services/api/realm.json", "{}\n"),
            frag("services/worker/realm.json", "{}\n"),
        ];
        let edges = DockerEdgeBuilder.build(&fragments, None);
        assert_eq!(
            targets_of(&edges, "services/api/docker-compose.yml"),
            vec!["services/api/realm.json".to_string()]
        );
    }

    /// Nothing at the declared path: the bare name is all there is to go on.
    #[test]
    fn a_path_that_does_not_exist_falls_back_to_the_bare_name() {
        let fragments = vec![
            frag("docker/Dockerfile", "FROM python:3.12\nCOPY app.py /srv/\n"),
            frag("src/app.py", "print(1)\n"),
        ];
        let edges = DockerEdgeBuilder.build(&fragments, None);
        assert_eq!(
            targets_of(&edges, "docker/Dockerfile"),
            vec!["src/app.py".to_string()]
        );
    }
}
