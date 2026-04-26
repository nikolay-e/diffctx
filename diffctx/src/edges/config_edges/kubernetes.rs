use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

const WEIGHT: f64 = 0.65;
const CONFIGMAP_SECRET_WEIGHT: f64 = 0.70;
const SERVICE_WEIGHT: f64 = 0.60;
const SELECTOR_WEIGHT: f64 = 0.55;
const IMAGE_WEIGHT: f64 = 0.40;
const REVERSE_FACTOR: f64 = 0.45;

static YAML_EXTS: Lazy<FxHashSet<&str>> = Lazy::new(|| [".yaml", ".yml"].iter().copied().collect());

static K8S_API_VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^apiVersion:\s?([^\s#]{1,100})").unwrap());
static K8S_KIND_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^kind:\s?(\w{1,100})").unwrap());
static K8S_METADATA_NAME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)^metadata:\s*\n\s{2,4}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static K8S_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap());
static CONFIGMAP_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)configMapKeyRef:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static CONFIGMAP_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)configMapName:\s?['"]?([^'"#\n]{1,200})"##).unwrap());
static SECRET_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)secretKeyRef:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static SECRET_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)secretName:\s?['"]?([^'"#\n]{1,200})"##).unwrap());

static SERVICE_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)serviceName:\s?['"]?([^'"#\n]{1,200})"##).unwrap());
static BACKEND_SERVICE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)service:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});

static IMAGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s{1,20}image:\s?['"]?([^'"#\n]{1,300})"##).unwrap());

static SELECTOR_MATCH_LABELS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)selector:\s?\n\s{1,20}matchLabels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})")
        .unwrap()
});
static LABELS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)labels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})")
        .unwrap()
});
static LABEL_PAIR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r##"(?m)^\s{0,20}([a-zA-Z0-9_./-]{1,100}):\s?['"]?([a-zA-Z0-9_./-]{1,100})['"]?\s{0,10}$"##,
    )
    .unwrap()
});
static SIMPLE_SELECTOR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)selector:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})")
        .unwrap()
});

static VOLUME_CONFIGMAP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)configMap:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static VOLUME_SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)secret:\s?\n\s{1,20}secretName:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static VOLUME_PVC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)persistentVolumeClaim:\s?\n\s{1,20}claimName:\s?['"]?([^'"#\n]{1,200})"##)
        .unwrap()
});

static K8S_KINDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "Deployment",
        "Service",
        "ConfigMap",
        "Secret",
        "Ingress",
        "Pod",
        "ReplicaSet",
        "StatefulSet",
        "DaemonSet",
        "Job",
        "CronJob",
        "PersistentVolume",
        "PersistentVolumeClaim",
        "ServiceAccount",
        "Role",
        "RoleBinding",
        "ClusterRole",
        "ClusterRoleBinding",
        "NetworkPolicy",
        "HorizontalPodAutoscaler",
        "Namespace",
    ]
    .iter()
    .copied()
    .collect()
});

static WORKLOAD_KINDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "Pod",
        "Deployment",
        "StatefulSet",
        "DaemonSet",
        "ReplicaSet",
        "Job",
        "CronJob",
    ]
    .iter()
    .copied()
    .collect()
});

fn is_yaml_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    YAML_EXTS.contains(ext.as_str())
}

fn is_kubernetes_manifest(path: &Path, content: &str) -> bool {
    if !is_yaml_file(path) {
        return false;
    }

    let api_match = K8S_API_VERSION_RE.is_match(content);
    let kind_match = K8S_KIND_RE.captures(content);

    if let (true, Some(cap)) = (api_match, kind_match) {
        let kind = cap[1].trim();
        K8S_KINDS.contains(kind)
    } else {
        false
    }
}

fn extract_resource_info(content: &str) -> (Option<String>, Option<String>) {
    let kind = K8S_KIND_RE
        .captures(content)
        .map(|c| c[1].trim().to_string());

    let name = K8S_METADATA_NAME_RE
        .captures(content)
        .or_else(|| K8S_NAME_RE.captures(content))
        .map(|c| c[1].trim().to_string());

    (kind, name)
}

fn extract_label_pairs(label_block: &str) -> FxHashMap<String, String> {
    let mut pairs = FxHashMap::default();
    for cap in LABEL_PAIR_RE.captures_iter(label_block) {
        pairs.insert(cap[1].trim().to_string(), cap[2].trim().to_string());
    }
    pairs
}

fn extract_labels_by_pattern(content: &str, pattern: &Regex) -> FxHashMap<String, String> {
    let mut labels = FxHashMap::default();
    for m in pattern.captures_iter(content) {
        labels.extend(extract_label_pairs(&m[1]));
    }
    labels
}

fn extract_labels(content: &str) -> FxHashMap<String, String> {
    extract_labels_by_pattern(content, &LABELS_RE)
}

fn extract_selector_labels(content: &str) -> FxHashMap<String, String> {
    extract_labels_by_pattern(content, &SELECTOR_MATCH_LABELS_RE)
}

fn labels_match(selector: &FxHashMap<String, String>, labels: &FxHashMap<String, String>) -> bool {
    if selector.is_empty() {
        return false;
    }
    selector
        .iter()
        .all(|(k, v)| labels.get(k).map_or(false, |lv| lv == v))
}

fn collect_k8s_dirs(k8s_files: &[&PathBuf]) -> FxHashSet<PathBuf> {
    let mut dirs = FxHashSet::default();
    let special_dirs: FxHashSet<&str> = ["base", "overlays", "templates", "manifests"]
        .iter()
        .copied()
        .collect();

    for f in k8s_files {
        if let Some(parent) = f.parent() {
            dirs.insert(parent.to_path_buf());
            if let Some(dir_name) = parent.file_name().and_then(|n| n.to_str()) {
                if special_dirs.contains(dir_name) {
                    if let Some(grandparent) = parent.parent() {
                        dirs.insert(grandparent.to_path_buf());
                    }
                }
            }
        }
    }
    dirs
}

fn is_in_k8s_dir(candidate: &Path, k8s_dirs: &FxHashSet<PathBuf>) -> bool {
    for dir in k8s_dirs {
        if candidate.starts_with(dir) {
            return true;
        }
    }
    false
}

struct K8sIndex {
    configmaps: FxHashMap<String, Vec<FragmentId>>,
    secrets: FxHashMap<String, Vec<FragmentId>>,
    services: FxHashMap<String, Vec<FragmentId>>,
    pvcs: FxHashMap<String, Vec<FragmentId>>,
    pods_with_labels: Vec<(FragmentId, FxHashMap<String, String>)>,
    images: FxHashMap<String, Vec<FragmentId>>,
}

impl K8sIndex {
    fn new() -> Self {
        Self {
            configmaps: FxHashMap::default(),
            secrets: FxHashMap::default(),
            services: FxHashMap::default(),
            pvcs: FxHashMap::default(),
            pods_with_labels: Vec::new(),
            images: FxHashMap::default(),
        }
    }
}

fn index_by_kind(kind: Option<&str>, name: &str, frag_id: &FragmentId, idx: &mut K8sIndex) {
    match kind {
        Some("ConfigMap") => idx
            .configmaps
            .entry(name.to_string())
            .or_default()
            .push(frag_id.clone()),
        Some("Secret") => idx
            .secrets
            .entry(name.to_string())
            .or_default()
            .push(frag_id.clone()),
        Some("Service") => idx
            .services
            .entry(name.to_string())
            .or_default()
            .push(frag_id.clone()),
        Some("PersistentVolumeClaim") => idx
            .pvcs
            .entry(name.to_string())
            .or_default()
            .push(frag_id.clone()),
        _ => {}
    }
}

fn index_images(frag: &Fragment, idx: &mut K8sIndex) {
    for cap in IMAGE_RE.captures_iter(&frag.content) {
        let image = cap[1].trim();
        if !image.is_empty() && !image.starts_with('$') {
            idx.images
                .entry(image.to_string())
                .or_default()
                .push(frag.id.clone());
        }
    }
}

fn index_fragment(frag: &Fragment, idx: &mut K8sIndex) {
    let (kind, name) = extract_resource_info(&frag.content);

    if let Some(ref n) = name {
        index_by_kind(kind.as_deref(), n, &frag.id, idx);
    }

    if let Some(ref k) = kind {
        if WORKLOAD_KINDS.contains(k.as_str()) {
            let labels = extract_labels(&frag.content);
            if !labels.is_empty() {
                idx.pods_with_labels.push((frag.id.clone(), labels));
            }
        }
    }

    index_images(frag, idx);
}

fn build_resource_index(k8s_fragments: &[&Fragment]) -> K8sIndex {
    let mut idx = K8sIndex::new();
    for frag in k8s_fragments {
        index_fragment(frag, &mut idx);
    }
    idx
}

fn link_by_patterns(
    frag: &Fragment,
    patterns: &[&Regex],
    index: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
    weight: f64,
) {
    for pattern in patterns {
        for cap in pattern.captures_iter(&frag.content) {
            let name = cap[1].trim();
            if let Some(target_ids) = index.get(name) {
                for target_id in target_ids {
                    if *target_id != frag.id {
                        add_edge(edges, &frag.id, target_id, weight, REVERSE_FACTOR);
                    }
                }
            }
        }
    }
}

fn build_configmap_edges(
    frag: &Fragment,
    configmaps: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    link_by_patterns(
        frag,
        &[&CONFIGMAP_REF_RE, &CONFIGMAP_NAME_RE, &VOLUME_CONFIGMAP_RE],
        configmaps,
        edges,
        CONFIGMAP_SECRET_WEIGHT,
    );
}

fn build_secret_edges(
    frag: &Fragment,
    secrets: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    link_by_patterns(
        frag,
        &[&SECRET_REF_RE, &SECRET_NAME_RE, &VOLUME_SECRET_RE],
        secrets,
        edges,
        CONFIGMAP_SECRET_WEIGHT,
    );
}

fn build_service_edges(
    frag: &Fragment,
    services: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    link_by_patterns(
        frag,
        &[&SERVICE_NAME_RE, &BACKEND_SERVICE_RE],
        services,
        edges,
        SERVICE_WEIGHT,
    );
}

fn build_volume_edges(
    frag: &Fragment,
    pvcs: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    link_by_patterns(frag, &[&VOLUME_PVC_RE], pvcs, edges, WEIGHT);
}

fn get_service_selector(content: &str) -> FxHashMap<String, String> {
    let selector = extract_selector_labels(content);
    if !selector.is_empty() {
        return selector;
    }

    match SIMPLE_SELECTOR_RE.captures(content) {
        Some(cap) => extract_label_pairs(&cap[1]),
        None => FxHashMap::default(),
    }
}

fn build_selector_edges(
    frag: &Fragment,
    pods_with_labels: &[(FragmentId, FxHashMap<String, String>)],
    edges: &mut EdgeDict,
) {
    let (kind, _) = extract_resource_info(&frag.content);
    if kind.as_deref() != Some("Service") {
        return;
    }

    let selector = get_service_selector(&frag.content);
    if selector.is_empty() {
        return;
    }

    for (pod_id, labels) in pods_with_labels {
        if *pod_id != frag.id && labels_match(&selector, labels) {
            add_edge(edges, &frag.id, pod_id, SELECTOR_WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn build_image_edges(
    frag: &Fragment,
    images: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    for cap in IMAGE_RE.captures_iter(&frag.content) {
        let image = cap[1].trim();
        if image.is_empty() || image.starts_with('$') {
            continue;
        }
        if let Some(other_ids) = images.get(image) {
            for other_id in other_ids {
                if *other_id != frag.id {
                    add_edge(edges, &frag.id, other_id, IMAGE_WEIGHT, REVERSE_FACTOR);
                }
            }
        }
    }
}

pub struct KubernetesEdgeBuilder;

impl EdgeBuilder for KubernetesEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let k8s_fragments: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_kubernetes_manifest(Path::new(f.path()), &f.content))
            .collect();

        if k8s_fragments.is_empty() {
            return EdgeDict::default();
        }

        let mut edges = EdgeDict::default();
        let idx = build_resource_index(&k8s_fragments);

        for frag in &k8s_fragments {
            build_configmap_edges(frag, &idx.configmaps, &mut edges);
            build_secret_edges(frag, &idx.secrets, &mut edges);
            build_service_edges(frag, &idx.services, &mut edges);
            build_volume_edges(frag, &idx.pvcs, &mut edges);
            build_selector_edges(frag, &idx.pods_with_labels, &mut edges);
            build_image_edges(frag, &idx.images, &mut edges);
        }

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        _repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let k8s_changed: Vec<&PathBuf> = changed
            .iter()
            .filter(|p| {
                if !is_yaml_file(p) {
                    return false;
                }
                match base::read_file_cached(p, file_cache) {
                    Some(content) => is_kubernetes_manifest(p, &content),
                    None => false,
                }
            })
            .collect();

        if k8s_changed.is_empty() {
            return vec![];
        }

        let k8s_dirs = collect_k8s_dirs(&k8s_changed);
        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut discovered = Vec::new();

        for candidate in candidates {
            if changed_set.contains(candidate) || !is_yaml_file(candidate) {
                continue;
            }
            if !is_in_k8s_dir(candidate, &k8s_dirs) {
                continue;
            }
            if let Some(content) = base::read_file_cached(candidate, file_cache) {
                if is_kubernetes_manifest(candidate, &content) {
                    discovered.push(candidate.clone());
                }
            }
        }

        discovered
    }
}
