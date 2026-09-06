use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::CICD;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, FragmentIndex, add_edge, link_by_name};

static GHA_RUN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s{0,20}-?\s{0,5}run:\s{0,5}[|>]?\s{0,5}([^\n]{1,500})").unwrap()
});
static GHA_RUN_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)run:\s*[|>]-?\s*\n((?:\s{2,}[^\n]*\n?)+)").unwrap());

static GITLAB_SCRIPT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s{0,20}(?:script|before_script|after_script):\s?\n((?:\s+-[^\n]*\n)+)")
        .unwrap()
});
static GITLAB_INCLUDE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)^\s{0,20}-?\s{0,5}(?:local|project|remote|template):\s{0,5}['"]?([^'"#\n]{1,300})"##)
        .unwrap()
});

static JENKINS_SH_DOUBLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)sh\s*\(?"([^"]*)""#).unwrap());
static JENKINS_SH_SINGLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)sh\s*\(?'([^']*)'").unwrap());
static JENKINS_SCRIPT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?ms)script\s*\{([^}]+)\}").unwrap());

static SCRIPT_CALL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\b(?:python3|python|dotnet|gradle|pytest|cargo|pnpm|yarn|bash|make|mypy|node|ruff|npm|mvn|go|sh)\s+([^\s;&|]+)",
    )
    .unwrap()
});

static FILE_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:\./|scripts/|bin/|tools/|src/|tests/)([a-zA-Z0-9_.-]+(?:\.(?:sh|py|js|ts|rb))?)",
    )
    .unwrap()
});

static PKG_MANAGER_SUBCOMMANDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    base::kw(concat!(
        "run test start build install ci init publish pack link unlink exec add remove upgrade info ",
        "list outdated audit fix cache config help version create set get why dedupe prune dlx ",
        "store ",
    ))
});
fn is_github_actions(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let lower = path_str.to_lowercase();
    (lower.contains(".github") && lower.contains("workflows"))
        && (lower.ends_with(".yml") || lower.ends_with(".yaml"))
}

fn is_gitlab_ci(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == ".gitlab-ci.yml" || name == ".gitlab-ci.yaml" || name.starts_with("gitlab-ci")
}

fn is_jenkinsfile(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "jenkinsfile" || name.ends_with(".jenkinsfile") || name.ends_with(".jenkins")
}

fn is_circleci(path: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    path_str.contains(".circleci") && (name == "config.yml" || name == "config.yaml")
}

fn is_travis(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == ".travis.yml" || name == ".travis.yaml"
}

fn is_azure_pipelines(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "azure-pipelines.yml"
        || name == "azure-pipelines.yaml"
        || name.starts_with("azure-pipeline")
}

fn is_tox(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "tox.ini"
}

fn is_nox(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "noxfile.py"
}

fn is_ci_file(path: &Path) -> bool {
    is_github_actions(path)
        || is_gitlab_ci(path)
        || is_jenkinsfile(path)
        || is_circleci(path)
        || is_travis(path)
        || is_azure_pipelines(path)
        || is_tox(path)
        || is_nox(path)
}

fn extract_script_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in SCRIPT_CALL_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            let script = m.as_str().trim().trim_matches(|c| c == '\'' || c == '"');
            if !script.is_empty()
                && !script.starts_with('-')
                && !PKG_MANAGER_SUBCOMMANDS.contains(script.to_lowercase().as_str())
            {
                refs.insert(script.to_string());
            }
        }
    }

    for cap in FILE_REF_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.insert(m.as_str().to_string());
        }
    }

    refs
}

fn extract_gha_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in GHA_RUN_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    for cap in GHA_RUN_BLOCK_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    refs
}

fn extract_gitlab_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in GITLAB_SCRIPT_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    for cap in GITLAB_INCLUDE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.insert(m.as_str().trim().to_string());
        }
    }

    refs
}

fn extract_jenkins_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in JENKINS_SH_DOUBLE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    for cap in JENKINS_SH_SINGLE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    for cap in JENKINS_SCRIPT_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.extend(extract_script_refs(m.as_str()));
        }
    }

    refs
}

fn extract_refs_for_content(path: &Path, content: &str) -> FxHashSet<String> {
    if is_github_actions(path) {
        extract_gha_refs(content)
    } else if is_gitlab_ci(path) {
        extract_gitlab_refs(content)
    } else if is_jenkinsfile(path) {
        extract_jenkins_refs(content)
    } else {
        extract_script_refs(content)
    }
}

fn add_package_json_ref(content: &str, refs: &mut FxHashSet<String>) {
    let lower = content.to_lowercase();
    if lower.contains("npm") || lower.contains("yarn") || lower.contains("pnpm") {
        refs.insert("package.json".to_string());
    }
}

pub struct CICDEdgeBuilder;

impl EdgeBuilder for CICDEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let ci_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_ci_file(Path::new(f.path())))
            .collect();
        if ci_frags.is_empty() {
            return EdgeDict::default();
        }

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges = EdgeDict::default();

        for ci in &ci_frags {
            let refs = extract_refs_for_content(Path::new(ci.path()), &ci.content);
            for r in &refs {
                link_by_name(
                    &ci.id,
                    r,
                    &idx,
                    &mut edges,
                    CICD.script_weight,
                    CICD.reverse_factor,
                );
            }

            let lower = ci.content.to_lowercase();
            if lower.contains("npm")
                || lower.contains("yarn")
                || lower.contains("pnpm")
                || lower.contains("npx")
            {
                // Every package.json in the repo used to qualify; in a
                // workspaces monorepo that is a hundred edges from one
                // workflow. The package a workflow drives is the nearest
                // one above it, or the root's.
                // Walked in the index's own key space (repo-relative): the
                // first version built absolute candidates from the fragment's
                // absolute path, matched nothing at every level, and at `/`
                // degraded to the bare name — every package.json again.
                // Exact path per level — `link_by_path_match`'s suffix rule
                // would let the root-level `package.json` name every
                // workspace's copy too.
                let rel = base::index_key_of(ci.path(), repo_root);
                let by_rel: FxHashMap<String, &FragmentId> = fragments
                    .iter()
                    .filter(|f| f.path().ends_with("package.json"))
                    .map(|f| (base::index_key_of(f.path(), repo_root), &f.id))
                    .collect();
                let reps = base::file_representatives(fragments);
                let mut dir = Path::new(&rel).parent();
                while let Some(d) = dir {
                    let candidate = if d.as_os_str().is_empty() {
                        "package.json".to_string()
                    } else {
                        format!("{}/package.json", d.to_string_lossy())
                    };
                    if let Some(fid) = by_rel.get(&candidate) {
                        let dst = reps.get(fid.path.as_ref()).unwrap_or(fid);
                        add_edge(
                            &mut edges,
                            &ci.id,
                            dst,
                            CICD.weight * CICD.script_modifier,
                            CICD.reverse_factor,
                        );
                        break;
                    }
                    if d.as_os_str().is_empty() {
                        break;
                    }
                    dir = d.parent();
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
        let ci_files: Vec<&PathBuf> = changed.iter().filter(|p| is_ci_file(p)).collect();
        if ci_files.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();

        for ci in &ci_files {
            let content = match base::read_file_cached(ci, file_cache) {
                Some(c) => c,
                None => continue,
            };

            let local_refs = extract_refs_for_content(ci, &content);
            refs.extend(local_refs);
            add_package_json_ref(&content, &mut refs);
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

    /// Production fragment paths are absolute and the index strips the repo
    /// root; the first version of this walk built absolute candidates,
    /// matched nothing at every level and at `/` fell back to the bare name —
    /// every package.json in the repo, which is what it set out to stop.
    #[test]
    fn a_workflow_links_to_the_nearest_package_json_only() {
        let root = "/repo";
        let fragments = vec![
            frag(
                "/repo/.github/workflows/ci.yml",
                "steps:\n  - run: npm ci && npm test\n",
            ),
            frag("/repo/package.json", "{\"name\": \"root\"}\n"),
            frag("/repo/packages/a/package.json", "{\"name\": \"a\"}\n"),
            frag("/repo/packages/b/package.json", "{\"name\": \"b\"}\n"),
        ];
        let edges = CICDEdgeBuilder.build(&fragments, Some(Path::new(root)));
        let targets: Vec<String> = edges
            .keys()
            .filter(|(src, _)| src.path.as_ref() == "/repo/.github/workflows/ci.yml")
            .map(|(_, dst)| dst.path.to_string())
            .collect();
        assert_eq!(
            targets,
            vec!["/repo/package.json".to_string()],
            "edges: {targets:?}"
        );
    }
}
