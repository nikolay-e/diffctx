use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::KUBERNETES;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

static YAML_EXTS: Lazy<FxHashSet<&str>> = Lazy::new(|| [".yaml", ".yml"].iter().copied().collect());

static K8S_API_VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^apiVersion:\s?([^\s#]{1,100})").unwrap());
static K8S_KIND_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^kind:\s?(\w{1,100})").unwrap());
static K8S_METADATA_NAME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)^metadata:\s*\n\s{2,4}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static K8S_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap());
static ENVFROM_CONFIGMAP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)configMapRef:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
static ENVFROM_SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)secretRef:\s?\n\s{1,20}name:\s?['"]?([^'"#\n]{1,200})"##).unwrap()
});
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
    Regex::new(r"(?m)selector:\s?\n\s{1,20}matchLabels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]+\n){1,50})")
        .unwrap()
});
static LABELS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)labels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]+\n){1,50})").unwrap()
});
static LABEL_PAIR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r##"(?m)^\s{0,20}([a-zA-Z0-9_./-]{1,100}):\s?['"]?([a-zA-Z0-9_./-]{1,100})['"]?\s{0,10}$"##,
    )
    .unwrap()
});
static SIMPLE_SELECTOR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)selector:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]+\n){1,50})")
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
    base::kw(concat!(
        "Deployment Service ConfigMap Secret Ingress Pod ReplicaSet StatefulSet DaemonSet Job ",
        "CronJob PersistentVolume PersistentVolumeClaim ServiceAccount Role RoleBinding ClusterRole ",
        "ClusterRoleBinding NetworkPolicy HorizontalPodAutoscaler Namespace ",
    ))
});
static WORKLOAD_KINDS: Lazy<FxHashSet<&str>> =
    Lazy::new(|| base::kw("Pod Deployment StatefulSet DaemonSet ReplicaSet Job CronJob"));
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

/// The resource kind and, with it, the byte offset at which the name was
/// declared: an edge into a manifest should land on the fragment that actually
/// carries the construct, and the offset is the only thing that can say which.
fn extract_resource_info(content: &str) -> (Option<String>, Option<(String, usize)>) {
    let kind = K8S_KIND_RE
        .captures(content)
        .map(|c| c[1].trim().to_string());

    let name = K8S_METADATA_NAME_RE
        .captures(content)
        .or_else(|| K8S_NAME_RE.captures(content))
        .and_then(|c| c.get(1))
        .map(|m| (m.as_str().trim().to_string(), m.start()));

    (kind, name)
}

/// A span of the manifest, as byte offsets into the reconstructed file text.
type Span = (usize, usize);

/// Label pairs carrying the span of the *block* they came from, not of the
/// single pair: a `labels:` block is the construct an edge is about, and
/// anchoring on it is what makes the edge land on a fragment worth reading
/// rather than on the one line the regex happened to match.
fn extract_label_pairs(label_block: &str, block: Span) -> Vec<(String, String, Span)> {
    LABEL_PAIR_RE
        .captures_iter(label_block)
        .map(|cap| (cap[1].trim().to_string(), cap[2].trim().to_string(), block))
        .collect()
}

type LabelMap = FxHashMap<String, (String, Span)>;

/// The lines of a captured block that belong to it: `(?:\s{1,20}k: v\n){1,50}`
/// keeps consuming any less-indented scalar line after the pairs — a Service's
/// `sessionAffinity:` and `type:` after its `selector:` — and every such line
/// became a REQUIRED selector key, so kubectl-ordered Services matched nothing.
/// A block ends at the first line indented less than its first.
fn own_block_end(block: &str) -> usize {
    let mut lines = block.split_inclusive('\n');
    let Some(first) = lines.next() else { return 0 };
    let indent = first.len() - first.trim_start().len();
    let mut end = first.len();
    for line in lines {
        if line.trim().is_empty() {
            end += line.len();
            continue;
        }
        if line.len() - line.trim_start().len() < indent {
            break;
        }
        end += line.len();
    }
    end
}

fn extract_labels_by_pattern(content: &str, pattern: &Regex) -> LabelMap {
    let mut labels = LabelMap::default();
    for cap in pattern.captures_iter(content) {
        let Some(block) = cap.get(1) else { continue };
        let own = &block.as_str()[..own_block_end(block.as_str())];
        let span = own_span(block.start(), own);
        for (key, value, _) in extract_label_pairs(own, span) {
            labels.insert(key, (value, span));
        }
    }
    labels
}

/// The span of a trimmed block: its last byte, never the newline after it.
fn own_span(start: usize, own: &str) -> Span {
    (start, (start + own.len()).saturating_sub(1).max(start))
}

fn extract_labels(content: &str) -> LabelMap {
    extract_labels_by_pattern(content, &LABELS_RE)
}

fn extract_selector_labels(content: &str) -> LabelMap {
    extract_labels_by_pattern(content, &SELECTOR_MATCH_LABELS_RE)
}

/// `Some(span)` when every selector pair is present with the same value; the
/// span covers the label blocks that matched, which is where the edge is
/// anchored. Matching semantics are unchanged — only the anchor is new.
fn labels_match(selector: &FxHashMap<String, String>, labels: &LabelMap) -> Option<Span> {
    if selector.is_empty() {
        return None;
    }
    let mut span = (usize::MAX, 0usize);
    for (k, v) in selector {
        match labels.get(k) {
            Some((lv, (start, end))) if lv == v => {
                span = (span.0.min(*start), span.1.max(*end));
            }
            _ => return None,
        }
    }
    Some(span)
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

/// One manifest as the engine has to read it: the file text rebuilt from its
/// fragments, plus the map back to them.
///
/// Two things made this builder emit nothing at all (#226). Detection ran per
/// fragment, and no fragment of a real manifest holds both `apiVersion:` and
/// `kind:` — so `is_kubernetes_manifest` was false for every fragment of every
/// manifest, and all six channels returned empty. And the multi-line patterns
/// here (`selector:` then `matchLabels:` then the pairs) only match against
/// whole-file text in the first place.
///
/// The text is reassembled by line number rather than by concatenation because
/// tree-sitter emits nested fragments for YAML — `spec:` 5-21 *and* `app: web`
/// 12-12 — and concatenating them would duplicate lines and put every offset
/// past the first nesting off by the length of its parent.
struct ManifestView {
    content: String,
    /// Byte offset where each line of `content` begins.
    line_starts: Vec<usize>,
    /// `(start_line, end_line, id)` per fragment, ascending.
    fragments: Vec<(u32, u32, FragmentId)>,
    /// The file's largest fragment: where a definition is worth reading (a
    /// `ConfigMap`'s `data:`, not its `metadata:`), and the fallback anchor.
    representative: FragmentId,
}

impl ManifestView {
    fn line_of(&self, offset: usize) -> u32 {
        self.line_starts
            .partition_point(|start| *start <= offset)
            .max(1) as u32
    }

    /// The smallest fragment that covers the whole matched construct, so an
    /// edge about a three-line `labels:` block lands on the `spec:` fragment
    /// that contains it and not on the one line the regex started at.
    fn anchor(&self, span: Span) -> &FragmentId {
        let (first, last) = (self.line_of(span.0), self.line_of(span.1));
        self.fragments
            .iter()
            .filter(|(start, end, _)| *start <= first && *end >= last)
            .min_by(|a, b| (a.1 - a.0, a.0, &a.2).cmp(&(b.1 - b.0, b.0, &b.2)))
            .map_or(&self.representative, |(_, _, id)| id)
    }
}

fn build_manifest_views(fragments: &[Fragment]) -> Vec<ManifestView> {
    let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
    for f in fragments {
        by_path.entry(f.path()).or_default().push(f);
    }

    let mut paths: Vec<&str> = by_path.keys().copied().collect();
    paths.sort_unstable();

    let mut views = Vec::new();
    for path in paths {
        // Before the reconstruction, not after: this builder sees every
        // fragment of every language, and rebuilding a whole file's text only
        // to discard it on the extension check cost one String per line of the
        // repository, per run.
        if !is_yaml_file(Path::new(path)) {
            continue;
        }
        let mut frags = by_path.remove(path).unwrap_or_default();
        frags.sort_by(|a, b| a.id.cmp(&b.id));

        let mut lines: Vec<String> = Vec::new();
        for f in &frags {
            for (i, line) in f.content.lines().enumerate() {
                let idx = (f.id.start_line as usize).saturating_sub(1) + i;
                if lines.len() <= idx {
                    lines.resize(idx + 1, String::new());
                }
                if lines[idx].is_empty() {
                    lines[idx] = line.to_string();
                }
            }
        }
        if lines.is_empty() {
            continue;
        }

        let mut content = String::new();
        let mut line_starts = Vec::with_capacity(lines.len());
        for line in &lines {
            line_starts.push(content.len());
            content.push_str(line);
            content.push('\n');
        }

        if !is_kubernetes_manifest(Path::new(path), &content) {
            continue;
        }

        // The crate's one definition of a file's representative (first
        // largest fragment), not a local tie-break that disagreed with it
        // whenever token counts were still zero.
        let owned: Vec<Fragment> = frags.iter().map(|f| (*f).clone()).collect();
        let Some(representative) = base::file_representatives(&owned).remove(path) else {
            continue;
        };

        views.push(ManifestView {
            content,
            line_starts,
            fragments: frags
                .iter()
                .map(|f| (f.id.start_line, f.id.end_line, f.id.clone()))
                .collect(),
            representative,
        });
    }
    views
}

/// A place inside a manifest: which view, and which span of it.
type Site = (usize, Span);

struct K8sIndex {
    configmaps: FxHashMap<String, Vec<FragmentId>>,
    secrets: FxHashMap<String, Vec<FragmentId>>,
    services: FxHashMap<String, Vec<FragmentId>>,
    pvcs: FxHashMap<String, Vec<FragmentId>>,
    pods_with_labels: Vec<(usize, LabelMap)>,
    images: FxHashMap<String, Vec<Site>>,
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
    let bucket = match kind {
        Some("ConfigMap") => &mut idx.configmaps,
        Some("Secret") => &mut idx.secrets,
        Some("Service") => &mut idx.services,
        Some("PersistentVolumeClaim") => &mut idx.pvcs,
        _ => return,
    };
    bucket
        .entry(name.to_string())
        .or_default()
        .push(frag_id.clone());
}

fn index_images(view_idx: usize, view: &ManifestView, idx: &mut K8sIndex) {
    for cap in IMAGE_RE.captures_iter(&view.content) {
        let Some(m) = cap.get(1) else { continue };
        let image = m.as_str().trim();
        if !image.is_empty() && !image.starts_with('$') {
            idx.images
                .entry(image.to_string())
                .or_default()
                .push((view_idx, (m.start(), m.end())));
        }
    }
}

fn index_view(view_idx: usize, view: &ManifestView, idx: &mut K8sIndex) {
    let (kind, name) = extract_resource_info(&view.content);

    if let Some((n, _)) = name {
        index_by_kind(kind.as_deref(), &n, &view.representative, idx);
    }

    if let Some(ref k) = kind {
        if WORKLOAD_KINDS.contains(k.as_str()) {
            let labels = extract_labels(&view.content);
            if !labels.is_empty() {
                idx.pods_with_labels.push((view_idx, labels));
            }
        }
    }

    index_images(view_idx, view, idx);
}

fn build_resource_index(views: &[ManifestView]) -> K8sIndex {
    let mut idx = K8sIndex::new();
    for (i, view) in views.iter().enumerate() {
        index_view(i, view, &mut idx);
    }
    idx
}

fn link_by_patterns(
    view: &ManifestView,
    patterns: &[&Regex],
    index: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
    weight: f64,
) {
    for pattern in patterns {
        for cap in pattern.captures_iter(&view.content) {
            let Some(m) = cap.get(1) else { continue };
            let name = m.as_str().trim();
            let Some(target_ids) = index.get(name) else {
                continue;
            };
            let src = view.anchor((m.start(), m.end()));
            for target_id in target_ids {
                if target_id != src {
                    add_edge(edges, src, target_id, weight, KUBERNETES.reverse_factor);
                }
            }
        }
    }
}

fn build_configmap_edges(view: &ManifestView, idx: &K8sIndex, edges: &mut EdgeDict) {
    link_by_patterns(
        view,
        &[
            &CONFIGMAP_REF_RE,
            &ENVFROM_CONFIGMAP_RE,
            &CONFIGMAP_NAME_RE,
            &VOLUME_CONFIGMAP_RE,
        ],
        &idx.configmaps,
        edges,
        KUBERNETES.configmap_secret_weight,
    );
}

fn build_secret_edges(view: &ManifestView, idx: &K8sIndex, edges: &mut EdgeDict) {
    link_by_patterns(
        view,
        &[
            &SECRET_REF_RE,
            &ENVFROM_SECRET_RE,
            &SECRET_NAME_RE,
            &VOLUME_SECRET_RE,
        ],
        &idx.secrets,
        edges,
        KUBERNETES.configmap_secret_weight,
    );
}

fn build_service_edges(view: &ManifestView, idx: &K8sIndex, edges: &mut EdgeDict) {
    link_by_patterns(
        view,
        &[&SERVICE_NAME_RE, &BACKEND_SERVICE_RE],
        &idx.services,
        edges,
        KUBERNETES.service_weight,
    );
}

fn build_volume_edges(view: &ManifestView, idx: &K8sIndex, edges: &mut EdgeDict) {
    link_by_patterns(view, &[&VOLUME_PVC_RE], &idx.pvcs, edges, KUBERNETES.weight);
}

/// The Service's selector labels and the span of the selector block.
fn get_service_selector(content: &str) -> (FxHashMap<String, String>, Span) {
    let flatten = |labels: LabelMap| {
        let start = labels.values().map(|(_, (s, _))| *s).min().unwrap_or(0);
        let end = labels.values().map(|(_, (_, e))| *e).max().unwrap_or(0);
        let map = labels
            .into_iter()
            .map(|(k, (v, _))| (k, v))
            .collect::<FxHashMap<_, _>>();
        (map, (start, end))
    };

    let selector = extract_selector_labels(content);
    if !selector.is_empty() {
        return flatten(selector);
    }

    match SIMPLE_SELECTOR_RE.captures(content).and_then(|c| c.get(1)) {
        Some(block) => {
            let own = &block.as_str()[..own_block_end(block.as_str())];
            let span = own_span(block.start(), own);
            let pairs = extract_label_pairs(own, span);
            (pairs.into_iter().map(|(k, v, _)| (k, v)).collect(), span)
        }
        None => (FxHashMap::default(), (0, 0)),
    }
}

fn build_selector_edges(
    views: &[ManifestView],
    view: &ManifestView,
    pods_with_labels: &[(usize, LabelMap)],
    edges: &mut EdgeDict,
) {
    let (kind, _) = extract_resource_info(&view.content);
    if kind.as_deref() != Some("Service") {
        return;
    }

    let (selector, selector_span) = get_service_selector(&view.content);
    if selector.is_empty() {
        return;
    }
    let src = view.anchor(selector_span);

    for (pod_view, labels) in pods_with_labels {
        if let Some(span) = labels_match(&selector, labels) {
            let dst = views[*pod_view].anchor(span);
            if dst != src {
                add_edge(
                    edges,
                    src,
                    dst,
                    KUBERNETES.selector_weight,
                    KUBERNETES.reverse_factor,
                );
            }
        }
    }
}

fn build_image_edges(
    views: &[ManifestView],
    view: &ManifestView,
    images: &FxHashMap<String, Vec<Site>>,
    edges: &mut EdgeDict,
) {
    for cap in IMAGE_RE.captures_iter(&view.content) {
        let Some(m) = cap.get(1) else { continue };
        let image = m.as_str().trim();
        if image.is_empty() || image.starts_with('$') {
            continue;
        }
        let Some(sites) = images.get(image) else {
            continue;
        };
        // A shared base image is vocabulary, not a relation: past the same bar
        // every other reference channel applies (MAX_FILES_PER_KEY), it would
        // link every workload in the cluster pairwise at naming weight.
        let mut distinct: Vec<usize> = sites.iter().map(|(v, _)| *v).collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() > super::generic::MAX_FILES_PER_KEY {
            continue;
        }
        let src = view.anchor((m.start(), m.end()));
        for (other_view, span) in sites {
            if std::ptr::eq(&views[*other_view], view) {
                continue;
            }
            let dst = views[*other_view].anchor(*span);
            if dst != src {
                add_edge(
                    edges,
                    src,
                    dst,
                    KUBERNETES.image_weight,
                    KUBERNETES.reverse_factor,
                );
            }
        }
    }
}

pub struct KubernetesEdgeBuilder;

impl EdgeBuilder for KubernetesEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let views = build_manifest_views(fragments);
        if views.is_empty() {
            return EdgeDict::default();
        }

        let mut edges = EdgeDict::default();
        let idx = build_resource_index(&views);

        for view in &views {
            build_configmap_edges(view, &idx, &mut edges);
            build_secret_edges(view, &idx, &mut edges);
            build_service_edges(view, &idx, &mut edges);
            build_volume_edges(view, &idx, &mut edges);
            build_selector_edges(&views, view, &idx.pods_with_labels, &mut edges);
            build_image_edges(&views, view, &idx.images, &mut edges);
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

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for issue #58: these label/selector patterns nest a Unicode
    // negated class under bounded repetition, which compiled past regex's
    // default 10 MiB size limit and made `.unwrap()` abort the whole process
    // the first time a workload manifest forced the Lazy static. Forcing every
    // k8s regex here fails CI on any future reintroduction instead of a user.
    #[test]
    fn all_kubernetes_regexes_compile() {
        for re in [
            &*K8S_API_VERSION_RE,
            &*K8S_KIND_RE,
            &*K8S_METADATA_NAME_RE,
            &*K8S_NAME_RE,
            &*CONFIGMAP_REF_RE,
            &*CONFIGMAP_NAME_RE,
            &*SECRET_REF_RE,
            &*SECRET_NAME_RE,
            &*SERVICE_NAME_RE,
            &*BACKEND_SERVICE_RE,
            &*IMAGE_RE,
            &*SELECTOR_MATCH_LABELS_RE,
            &*LABELS_RE,
            &*LABEL_PAIR_RE,
            &*SIMPLE_SELECTOR_RE,
            &*VOLUME_CONFIGMAP_RE,
            &*VOLUME_SECRET_RE,
            &*VOLUME_PVC_RE,
        ] {
            let _ = re.is_match("x");
        }
    }

    fn yaml_frag(path: &str, start: u32, body: &str) -> Fragment {
        let end = start + body.lines().count().max(1) as u32 - 1;
        Fragment {
            id: crate::types::FragmentId::new(std::sync::Arc::from(path), start, end),
            kind: crate::types::FragmentKind::Chunk,
            content: std::sync::Arc::from(body),
            identifiers: FxHashSet::default(),
            token_count: body.len() as u32,
            symbol_name: None,
        }
    }

    /// #226: `parsers/config_parser.rs` splits a YAML file into one fragment
    /// per top-level key, so no fragment of a real manifest holds both
    /// `apiVersion:` and `kind:` — the per-fragment filter was false for all
    /// of them and every channel in this builder emitted nothing. Detection
    /// runs on the whole file now; the edge still lands on the fragment that
    /// carries the matching label.
    #[test]
    fn a_manifest_split_across_fragments_still_links_service_to_workload() {
        let fragments = vec![
            yaml_frag("k8s/deployment.yaml", 1, "apiVersion: apps/v1\n"),
            yaml_frag("k8s/deployment.yaml", 2, "kind: Deployment\n"),
            yaml_frag("k8s/deployment.yaml", 3, "metadata:\n  name: web\n"),
            yaml_frag(
                "k8s/deployment.yaml",
                5,
                "spec:\n  template:\n    metadata:\n      labels:\n        app: web\n        tier: frontend\n",
            ),
            // Bigger than the `spec:` fragment on purpose: it makes this file's
            // `representative` a fragment that contains no labels, so an edge
            // landing on `spec:` proves the anchor resolved the block rather
            // than falling through to the fallback. Without it the assertion
            // below holds either way, which is how the span off-by-one shipped.
            yaml_frag(
                "k8s/deployment.yaml",
                11,
                "status:\n  observedGeneration: 1\n  replicas: 3\n  readyReplicas: 3\n  updatedReplicas: 3\n  availableReplicas: 3\n  conditions: []\n",
            ),
            yaml_frag("k8s/service.yaml", 1, "apiVersion: v1\n"),
            yaml_frag("k8s/service.yaml", 2, "kind: Service\n"),
            yaml_frag("k8s/service.yaml", 3, "metadata:\n  name: web\n"),
            yaml_frag("k8s/service.yaml", 5, "spec:\n  selector:\n    app: web\n"),
        ];

        let edges = KubernetesEdgeBuilder.build(&fragments, None);
        assert!(!edges.is_empty(), "the selector channel emitted nothing");

        let forward = edges
            .keys()
            .find(|(src, dst)| {
                src.path.as_ref() == "k8s/service.yaml"
                    && dst.path.as_ref() == "k8s/deployment.yaml"
            })
            .expect("no service -> deployment edge");
        let rep = build_manifest_views(&fragments)
            .iter()
            .find(|v| v.fragments[0].2.path.as_ref() == "k8s/deployment.yaml")
            .map(|v| v.representative.clone())
            .expect("the deployment must produce a view");
        assert_eq!(
            (rep.start_line, rep.end_line),
            (11, 17),
            "the fixture must make `status:` the representative, or the assertion below proves nothing"
        );
        assert_eq!(
            (forward.1.start_line, forward.1.end_line),
            (5, 10),
            "the edge must land on the fragment carrying the matched label, not on the fallback"
        );
    }
}
