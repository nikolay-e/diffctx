use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::analytics::ANALYTICS;
use crate::graph::{EdgeCategory, Graph};
use crate::types::{Fragment, FragmentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotientLevel {
    Fragment,
    File,
    Directory,
}

impl QuotientLevel {
    // The only caller is the Python bridge, and its argument comes from a
    // user. A `_ => Directory` fallback turned `level="modules"` into a valid
    // query for something else and returned it without a word.
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "fragment" => Some(Self::Fragment),
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuotientNode {
    pub key: Arc<str>,
    pub label: String,
    pub fragment_count: u32,
    pub token_count: u64,
    pub self_weight: f64,
}

#[derive(Debug, Clone)]
pub struct QuotientEdge {
    pub source: Arc<str>,
    pub target: Arc<str>,
    pub weight: f64,
    pub categories: FxHashMap<EdgeCategory, u32>,
}

#[derive(Debug, Clone)]
pub struct QuotientGraph {
    pub nodes: FxHashMap<Arc<str>, QuotientNode>,
    pub edges: FxHashMap<(Arc<str>, Arc<str>), QuotientEdge>,
}

impl QuotientGraph {
    pub fn new() -> Self {
        Self {
            nodes: FxHashMap::default(),
            edges: FxHashMap::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModuleMetrics {
    pub name: Arc<str>,
    pub cohesion: f64,
    pub coupling: f64,
    pub instability: f64,
    pub fan_in: u32,
    pub fan_out: u32,
}

#[derive(Debug, Clone)]
pub struct HotspotEntry {
    pub path: Arc<str>,
    pub score: f64,
    pub out_degree: u32,
}

// Fragment ids are built from `file_path.to_string_lossy()`, so on Windows
// they carry `\` separators. These three used to be hand-rolled string
// operations over `/` alone: `strip_prefix(root)` then left a leading `\`,
// `parent` found no separator and returned "", and every file collapsed into
// the single "." directory group. Path-aware splitting plus one
// `to_posix_display` at the boundary keeps the emitted keys POSIX-shaped on
// every platform, which is what `graph_export.rs` already did — the two
// spellings of `relative_path` disagreeing is what made this reachable.
fn relative_path(path: &str, root: Option<&str>) -> String {
    let Some(root) = root.filter(|r| !r.is_empty()) else {
        return crate::paths::to_posix_display(std::borrow::Cow::Borrowed(path));
    };
    let p = Path::new(path);
    match p.strip_prefix(Path::new(root)) {
        Ok(rel) => crate::paths::to_posix_display(rel.to_string_lossy()),
        Err(_) => crate::paths::to_posix_display(std::borrow::Cow::Borrowed(path)),
    }
}

fn basename(s: &str) -> &str {
    Path::new(s)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(s)
}

fn parent(s: &str) -> &str {
    Path::new(s)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|p| !p.is_empty())
        .unwrap_or("")
}

fn group_key(fid: &FragmentId, level: QuotientLevel, root: Option<&str>) -> Arc<str> {
    let rel = relative_path(fid.path.as_ref(), root);
    match level {
        QuotientLevel::Fragment => {
            Arc::from(format!("{}:{}-{}", rel, fid.start_line, fid.end_line).as_str())
        }
        QuotientLevel::File => Arc::from(rel),
        QuotientLevel::Directory => {
            let p = parent(&rel);
            if p.is_empty() {
                Arc::from(".")
            } else {
                Arc::from(p)
            }
        }
    }
}

fn node_label(fid: &FragmentId, frag: &Fragment, level: QuotientLevel, key: &str) -> String {
    match level {
        QuotientLevel::Fragment => {
            let bn = basename(fid.path.as_ref());
            if let Some(name) = frag.symbol_name.as_deref() {
                format!("{} ({}:{})", name, bn, fid.start_line)
            } else {
                format!("{}:{}-{}", bn, fid.start_line, fid.end_line)
            }
        }
        QuotientLevel::File => basename(fid.path.as_ref()).to_string(),
        QuotientLevel::Directory => {
            let trimmed = key.trim_end_matches('/');
            let bn = basename(trimmed);
            if bn.is_empty() {
                ".".to_string()
            } else {
                bn.to_string()
            }
        }
    }
}

fn iter_forward_edges<F: FnMut(&FragmentId, &FragmentId, f64)>(graph: &Graph, mut f: F) {
    let fwd = match graph.fwd_csr() {
        Some(c) => c,
        None => return,
    };
    for src_idx in 0..fwd.n {
        let s = fwd.indptr[src_idx] as usize;
        let e = fwd.indptr[src_idx + 1] as usize;
        let src = &fwd.idx_to_node[src_idx];
        for k in s..e {
            let dst_idx = fwd.indices[k] as usize;
            let dst = &fwd.idx_to_node[dst_idx];
            f(src, dst, fwd.weights[k]);
        }
    }
}

// `edge_types` filters the FRAGMENT edges, before they are aggregated into
// quotient edges. Filtering afterwards was wrong in two ways that no caller
// could see: `QuotientEdge::weight` is one sum over every category, so
// admitting an edge because one of its categories matched contributed the
// whole sum (semantic=1 + history=9 reported coupling 10 under
// `edge_types=["semantic"]`), and `self_weight` was never filtered at all,
// so cohesion always mixed every category regardless of the argument.
pub fn quotient_graph(
    graph: &Graph,
    fragments: &[Fragment],
    level: QuotientLevel,
    root: Option<&str>,
    edge_types: Option<&FxHashSet<EdgeCategory>>,
) -> QuotientGraph {
    let mut qg = QuotientGraph::new();

    let mut fid_to_group: FxHashMap<FragmentId, Arc<str>> = FxHashMap::default();
    for frag in fragments {
        let key = group_key(&frag.id, level, root);
        fid_to_group.insert(frag.id.clone(), key.clone());

        let entry = qg.nodes.entry(key.clone()).or_insert_with(|| QuotientNode {
            key: key.clone(),
            label: node_label(&frag.id, frag, level, key.as_ref()),
            fragment_count: 0,
            token_count: 0,
            self_weight: 0.0,
        });
        entry.fragment_count += 1;
        entry.token_count += u64::from(frag.token_count);
    }

    iter_forward_edges(graph, |src, dst, weight| {
        let src_key = match fid_to_group.get(src) {
            Some(k) => k.clone(),
            None => return,
        };
        let dst_key = match fid_to_group.get(dst) {
            Some(k) => k.clone(),
            None => return,
        };
        let cat = graph
            .edge_category(src, dst)
            .unwrap_or(EdgeCategory::Generic);

        if let Some(filter) = edge_types
            && !filter.contains(&cat)
        {
            return;
        }

        if src_key == dst_key {
            if let Some(node) = qg.nodes.get_mut(&src_key) {
                node.self_weight += weight;
            }
        } else {
            let pair = (src_key.clone(), dst_key.clone());
            let edge = qg.edges.entry(pair).or_insert_with(|| QuotientEdge {
                source: src_key,
                target: dst_key,
                weight: 0.0,
                categories: FxHashMap::default(),
            });
            edge.weight += weight;
            *edge.categories.entry(cat).or_insert(0) += 1;
        }
    });

    qg
}

pub fn coupling_metrics(
    graph: &Graph,
    fragments: &[Fragment],
    level: QuotientLevel,
    root: Option<&str>,
    edge_types: Option<&FxHashSet<EdgeCategory>>,
) -> Vec<ModuleMetrics> {
    let qg = quotient_graph(graph, fragments, level, root, edge_types);

    let mut out_weight: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    let mut in_weight: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    let mut fan_in_set: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();
    let mut fan_out_set: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();

    for ((src, dst), edge) in &qg.edges {
        *out_weight.entry(src.clone()).or_insert(0.0) += edge.weight;
        *in_weight.entry(dst.clone()).or_insert(0.0) += edge.weight;
        fan_out_set
            .entry(src.clone())
            .or_default()
            .insert(dst.clone());
        fan_in_set
            .entry(dst.clone())
            .or_default()
            .insert(src.clone());
    }

    let mut keys: Vec<Arc<str>> = qg.nodes.keys().cloned().collect();
    keys.sort();

    let mut results = Vec::with_capacity(keys.len());
    for key in keys {
        let node = &qg.nodes[&key];
        let intra = node.self_weight;
        let inter = out_weight.get(&key).copied().unwrap_or(0.0)
            + in_weight.get(&key).copied().unwrap_or(0.0);
        let total = intra + inter;
        let cohesion = if total > 0.0 { intra / total } else { 0.0 };
        let coupling = if total > 0.0 { inter / total } else { 0.0 };
        let fi = fan_in_set.get(&key).map_or(0, |s| s.len()) as u32;
        let fo = fan_out_set.get(&key).map_or(0, |s| s.len()) as u32;
        let denom = fi + fo;
        let instability = if denom > 0 {
            f64::from(fo) / f64::from(denom)
        } else {
            0.0
        };

        results.push(ModuleMetrics {
            name: key,
            cohesion: round3(cohesion),
            coupling: round3(coupling),
            instability: round3(instability),
            fan_in: fi,
            fan_out: fo,
        });
    }

    results
}

pub fn hotspots(
    graph: &Graph,
    fragments: &[Fragment],
    top: usize,
    root: Option<&str>,
    edge_types: Option<&FxHashSet<EdgeCategory>>,
) -> Vec<HotspotEntry> {
    let mut file_frag_count: FxHashMap<Arc<str>, u32> = FxHashMap::default();
    for frag in fragments {
        let rel: Arc<str> = Arc::from(relative_path(frag.id.path.as_ref(), root));
        *file_frag_count.entry(rel).or_insert(0) += 1;
    }

    // Fan-out is counted over distinct DESTINATION FILES, not over fragment
    // edges. Counting edges made a finely fragmented file look hotter than a
    // coarse one with the same real fan-out — the score then ranked the
    // fragmenter's output rather than the code's coupling.
    let mut out_targets: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, cat| {
        if let Some(filter) = edge_types
            && !filter.contains(&cat)
        {
            return;
        }
        let src_rel: Arc<str> = Arc::from(relative_path(src.path.as_ref(), root));
        let dst_rel: Arc<str> = Arc::from(relative_path(dst.path.as_ref(), root));
        if src_rel == dst_rel {
            return;
        }
        out_targets.entry(src_rel).or_default().insert(dst_rel);
    });

    let out_deg: FxHashMap<Arc<str>, u32> = out_targets
        .into_iter()
        .map(|(k, v)| (k, v.len() as u32))
        .collect();

    // With no edges every file scores 0, the tie breaks alphabetically, and
    // the first `top` files came back indistinguishable from a real ranking.
    // No signal is an empty answer, not an arbitrary one.
    let Some(max_deg) = out_deg.values().copied().max().filter(|m| *m > 0) else {
        return Vec::new();
    };

    let mut scored: Vec<HotspotEntry> = file_frag_count
        .into_keys()
        .map(|file| {
            let deg = out_deg.get(&file).copied().unwrap_or(0);
            let deg_norm = f64::from(deg) / f64::from(max_deg);
            let score = round4(ANALYTICS.hotspot_degree_weight * deg_norm);
            HotspotEntry {
                path: file,
                score,
                out_degree: deg,
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.as_ref().cmp(b.path.as_ref()))
    });
    scored.truncate(top);
    scored
}

// The mermaid text is a re-parsed protocol, not just a rendering artifact:
// `src/diffctx/_native/graph_analytics.py`'s `_MERMAID_NODE_LINE` regex
// (`^\s*(n\d+)\["(.*)"\]\s*$`) drives cycle detection by matching the quote
// that closes the label. A path or symbol name containing `"`, `[` or `]`
// (all legal on POSIX, and `[`/`]` are valid in most identifiers too) is
// also invalid inside a mermaid quoted label, so it can corrupt the whole
// diagram for a real mermaid renderer. Escape with mermaid's own `#NNN;`
// numeric-character-reference syntax so the label round-trips as plain text
// with no bare delimiter characters.
fn escape_mermaid_label(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '#' => out.push_str("#35;"),
            '"' => out.push_str("#quot;"),
            '[' => out.push_str("#91;"),
            ']' => out.push_str("#93;"),
            other => out.push(other),
        }
    }
    out
}

pub fn to_mermaid(qg: &QuotientGraph, top_n: usize) -> String {
    if qg.nodes.is_empty() {
        return "graph LR\n".to_string();
    }

    let mut node_total_weight: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    for node in qg.nodes.values() {
        node_total_weight.insert(node.key.clone(), node.self_weight);
    }
    for edge in qg.edges.values() {
        if let Some(v) = node_total_weight.get_mut(&edge.source) {
            *v += edge.weight;
        }
        if let Some(v) = node_total_weight.get_mut(&edge.target) {
            *v += edge.weight;
        }
    }

    let mut sorted_nodes: Vec<&QuotientNode> = qg.nodes.values().collect();
    sorted_nodes.sort_by(|a, b| {
        let aw = node_total_weight.get(&a.key).copied().unwrap_or(0.0);
        let bw = node_total_weight.get(&b.key).copied().unwrap_or(0.0);
        bw.partial_cmp(&aw)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.as_ref().cmp(b.key.as_ref()))
    });
    sorted_nodes.truncate(top_n);

    let node_keys: FxHashSet<Arc<str>> = sorted_nodes.iter().map(|n| n.key.clone()).collect();
    let node_ids: FxHashMap<Arc<str>, String> = sorted_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.key.clone(), format!("n{i}")))
        .collect();

    let mut lines: Vec<String> = vec!["graph LR".to_string()];
    for node in &sorted_nodes {
        let nid = &node_ids[&node.key];
        let trimmed = node.key.trim_end_matches('/');
        let fallback = if trimmed.is_empty() { "root" } else { trimmed };
        let label = if node.label.is_empty() {
            fallback
        } else {
            node.label.as_str()
        };
        let label = escape_mermaid_label(label);
        lines.push(format!("    {nid}[\"{label}\"]"));
    }

    let mut sorted_edges: Vec<&QuotientEdge> = qg.edges.values().collect();
    sorted_edges.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source.as_ref().cmp(b.source.as_ref()))
            .then_with(|| a.target.as_ref().cmp(b.target.as_ref()))
    });

    for edge in sorted_edges {
        if !node_keys.contains(&edge.source) || !node_keys.contains(&edge.target) {
            continue;
        }
        let src_id = &node_ids[&edge.source];
        let dst_id = &node_ids[&edge.target];
        let top_cat = edge
            .categories
            .iter()
            .max_by_key(|&(_, count)| *count)
            .map_or("?", |(c, _)| c.as_str());
        let weight_str = format_weight(edge.weight);
        lines.push(format!(
            "    {src_id} -->|\"{top_cat}: {weight_str}\"| {dst_id}"
        ));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn format_weight(w: f64) -> String {
    if (w - w.round()).abs() < f64::EPSILON {
        format!("{}", w as i64)
    } else {
        format!("{w:.1}")
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// The `is_finite` guard is not decoration: without it a NaN score became
// NaN * 10000 rounded, and this module and `graph_export.rs` returned
// different values for the same input.
fn round4(v: f64) -> f64 {
    if v.is_finite() {
        (v * 10000.0).round() / 10000.0
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;
    use regex::Regex;

    fn fid(path: &str, start: u32, end: u32) -> FragmentId {
        FragmentId::new(Arc::from(path), start, end)
    }

    fn frag(path: &str, start: u32, end: u32, tokens: u32) -> Fragment {
        Fragment {
            id: fid(path, start, end),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: tokens,
            symbol_name: None,
        }
    }

    fn build(
        edges: &[(FragmentId, FragmentId, f64, EdgeCategory)],
        fragments: &[Fragment],
    ) -> Graph {
        let mut g = Graph::new();
        for f in fragments {
            g.add_node(f.id.clone());
        }
        for (s, d, w, c) in edges {
            g.add_edge(s.clone(), d.clone(), *w);
            g.insert_edge_category(s.clone(), d.clone(), *c);
        }
        g.freeze();
        g
    }

    #[test]
    fn hotspots_returns_top_k_sorted() {
        let frags = vec![
            frag("a.rs", 1, 5, 10),
            frag("b.rs", 1, 5, 10),
            frag("c.rs", 1, 5, 10),
        ];
        let edges = vec![
            (
                frags[0].id.clone(),
                frags[1].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[0].id.clone(),
                frags[2].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[1].id.clone(),
                frags[2].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
        ];
        let g = build(&edges, &frags);
        let hs = hotspots(&g, &frags, 2, None, None);
        assert_eq!(hs.len(), 2);
        assert_eq!(hs[0].path.as_ref(), "a.rs");
        assert!(hs[0].score >= hs[1].score);
    }

    #[test]
    fn coupling_metrics_disconnected_zero_coupling() {
        let frags = vec![frag("dirA/a.rs", 1, 5, 10), frag("dirB/b.rs", 1, 5, 10)];
        let edges: Vec<(FragmentId, FragmentId, f64, EdgeCategory)> = Vec::new();
        let g = build(&edges, &frags);
        let metrics = coupling_metrics(&g, &frags, QuotientLevel::Directory, None, None);
        assert_eq!(metrics.len(), 2);
        for m in &metrics {
            assert!((m.cohesion - 0.0).abs() < 1e-9);
            assert!((m.coupling - 0.0).abs() < 1e-9);
            assert_eq!(m.fan_in, 0);
            assert_eq!(m.fan_out, 0);
        }
    }

    #[test]
    fn quotient_graph_trivial_partition_collapses_to_directories() {
        let frags = vec![
            frag("dirA/a.rs", 1, 5, 100),
            frag("dirA/b.rs", 1, 5, 50),
            frag("dirB/c.rs", 1, 5, 200),
        ];
        let edges = vec![
            (
                frags[0].id.clone(),
                frags[1].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[0].id.clone(),
                frags[2].id.clone(),
                2.0,
                EdgeCategory::Semantic,
            ),
        ];
        let g = build(&edges, &frags);
        let qg = quotient_graph(&g, &frags, QuotientLevel::Directory, None, None);
        assert_eq!(qg.nodes.len(), 2);
        let dir_a: Arc<str> = Arc::from("dirA");
        let dir_b: Arc<str> = Arc::from("dirB");
        assert!(qg.nodes.contains_key(&dir_a));
        assert!(qg.nodes.contains_key(&dir_b));
        assert_eq!(qg.nodes[&dir_a].fragment_count, 2);
        assert_eq!(qg.nodes[&dir_a].token_count, 150);
        assert!((qg.nodes[&dir_a].self_weight - 1.0).abs() < 1e-9);
        let cross = (dir_a.clone(), dir_b.clone());
        assert!(qg.edges.contains_key(&cross));
        assert!((qg.edges[&cross].weight - 2.0).abs() < 1e-9);
    }

    #[test]
    fn mermaid_round_trip_contains_nodes_and_edges() {
        let frags = vec![frag("dirA/a.rs", 1, 5, 10), frag("dirB/b.rs", 1, 5, 10)];
        let edges = vec![(
            frags[0].id.clone(),
            frags[1].id.clone(),
            3.0,
            EdgeCategory::Structural,
        )];
        let g = build(&edges, &frags);
        let qg = quotient_graph(&g, &frags, QuotientLevel::Directory, None, None);
        let mermaid = to_mermaid(&qg, 20);
        assert!(mermaid.starts_with("graph LR"));
        assert!(mermaid.contains("dirA"));
        assert!(mermaid.contains("dirB"));
        assert!(mermaid.contains("structural: 3"));
        assert!(mermaid.ends_with('\n'));
    }

    #[test]
    fn mermaid_escapes_quotes_and_brackets_in_node_labels() {
        // Mirrors `src/diffctx/_native/graph_analytics.py`'s
        // `_MERMAID_NODE_LINE = re.compile(r'^\s*(n\d+)\["(.*)"\]\s*$')`,
        // which re-parses this text to drive cycle detection.
        let mermaid_node_line = Regex::new(r#"^\s*(n\d+)\["(.*)"\]\s*$"#).unwrap();

        let frags = vec![frag("src/say\"hi\"[x].rs", 1, 10, 7)];
        let g = build(&[], &frags);
        let qg = quotient_graph(&g, &frags, QuotientLevel::File, None, None);
        let mermaid = to_mermaid(&qg, 20);

        let node_line = mermaid
            .lines()
            .find(|l| l.contains("n0"))
            .expect("node line for n0 must be present");
        let caps = mermaid_node_line
            .captures(node_line)
            .unwrap_or_else(|| panic!("node line does not match mermaid grammar: {node_line:?}"));
        let label = &caps[2];
        assert!(
            !label.contains('"'),
            "escaped label still contains a bare quote: {label:?}"
        );
    }

    #[test]
    fn mermaid_empty_graph() {
        let qg = QuotientGraph::new();
        assert_eq!(to_mermaid(&qg, 20), "graph LR\n");
    }

    #[test]
    fn coupling_filter_excludes_the_weight_of_filtered_categories() {
        let frags = vec![frag("a/x.rs", 1, 5, 10), frag("b/y.rs", 1, 5, 10)];
        let edges = vec![
            (
                frags[0].id.clone(),
                frags[1].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[1].id.clone(),
                frags[0].id.clone(),
                9.0,
                EdgeCategory::History,
            ),
        ];
        let g = build(&edges, &frags);

        let mut only_semantic = FxHashSet::default();
        only_semantic.insert(EdgeCategory::Semantic);
        let filtered = coupling_metrics(
            &g,
            &frags,
            QuotientLevel::Directory,
            None,
            Some(&only_semantic),
        );
        let unfiltered = coupling_metrics(&g, &frags, QuotientLevel::Directory, None, None);

        // The history edge carries 9 of the 10 units of weight between these
        // two modules. Filtering to `semantic` must not report it.
        let fan_out_filtered: u32 = filtered.iter().map(|m| m.fan_out).sum();
        let fan_out_unfiltered: u32 = unfiltered.iter().map(|m| m.fan_out).sum();
        assert_eq!(fan_out_unfiltered, 2);
        assert_eq!(fan_out_filtered, 1);
    }

    #[test]
    fn cohesion_ignores_intra_module_edges_of_filtered_categories() {
        let frags = vec![frag("a/x.rs", 1, 5, 10), frag("a/y.rs", 1, 5, 10)];
        let edges = vec![(
            frags[0].id.clone(),
            frags[1].id.clone(),
            5.0,
            EdgeCategory::History,
        )];
        let g = build(&edges, &frags);

        let mut only_semantic = FxHashSet::default();
        only_semantic.insert(EdgeCategory::Semantic);
        let filtered = coupling_metrics(
            &g,
            &frags,
            QuotientLevel::Directory,
            None,
            Some(&only_semantic),
        );
        // self_weight used to accumulate regardless of the filter, so a module
        // whose only edge was excluded still reported perfect cohesion.
        assert!(filtered.iter().all(|m| m.cohesion == 0.0));
    }

    #[test]
    fn hotspots_on_a_graph_with_no_edges_is_empty_not_alphabetical() {
        let frags = vec![
            frag("a.rs", 1, 5, 10),
            frag("b.rs", 1, 5, 10),
            frag("c.rs", 1, 5, 10),
        ];
        let g = build(&[], &frags);
        assert!(hotspots(&g, &frags, 10, None, None).is_empty());
    }

    #[test]
    fn hotspot_fan_out_counts_files_not_fragment_edges() {
        // `many.rs` is split into two fragments that both point at the same
        // file; `one.rs` points at two distinct files. Counting fragment edges
        // tied them; counting destination files ranks `one.rs` higher.
        let frags = vec![
            frag("many.rs", 1, 5, 10),
            frag("many.rs", 6, 10, 10),
            frag("one.rs", 1, 5, 10),
            frag("t1.rs", 1, 5, 10),
            frag("t2.rs", 1, 5, 10),
        ];
        let edges = vec![
            (
                frags[0].id.clone(),
                frags[3].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[1].id.clone(),
                frags[3].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[2].id.clone(),
                frags[3].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[2].id.clone(),
                frags[4].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
        ];
        let g = build(&edges, &frags);
        let out = hotspots(&g, &frags, 10, None, None);
        let by_path: FxHashMap<&str, u32> = out
            .iter()
            .map(|e| (e.path.as_ref(), e.out_degree))
            .collect();
        assert_eq!(by_path["many.rs"], 1);
        assert_eq!(by_path["one.rs"], 2);
        assert_eq!(out[0].path.as_ref(), "one.rs");
    }

    #[test]
    fn unknown_level_and_category_strings_are_rejected() {
        assert!(QuotientLevel::try_from_str("modules").is_none());
        assert_eq!(
            QuotientLevel::try_from_str("directory"),
            Some(QuotientLevel::Directory)
        );
        assert!(EdgeCategory::try_from_str("semantics").is_none());
        assert_eq!(
            EdgeCategory::try_from_str("semantic"),
            Some(EdgeCategory::Semantic)
        );
        assert_eq!(
            EdgeCategory::try_from_str("generic"),
            Some(EdgeCategory::Generic)
        );
    }
}
