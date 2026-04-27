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
    pub fn from_str(s: &str) -> Self {
        match s {
            "fragment" => Self::Fragment,
            "file" => Self::File,
            _ => Self::Directory,
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
    pub level: QuotientLevel,
}

impl QuotientGraph {
    pub fn new(level: QuotientLevel) -> Self {
        Self {
            nodes: FxHashMap::default(),
            edges: FxHashMap::default(),
            level,
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
    pub churn: u32,
}

fn relative_path<'a>(path: &'a str, root: Option<&str>) -> &'a str {
    let root = match root {
        Some(r) if !r.is_empty() => r,
        _ => return path,
    };
    if let Some(stripped) = path.strip_prefix(root) {
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        path
    }
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn parent(s: &str) -> &str {
    match s.rfind('/') {
        Some(i) => &s[..i],
        None => "",
    }
}

fn group_key(fid: &FragmentId, level: QuotientLevel, root: Option<&str>) -> Arc<str> {
    let rel = relative_path(fid.path.as_ref(), root);
    match level {
        QuotientLevel::Fragment => {
            Arc::from(format!("{}:{}-{}", rel, fid.start_line, fid.end_line).as_str())
        }
        QuotientLevel::File => Arc::from(rel),
        QuotientLevel::Directory => {
            let p = parent(rel);
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

pub fn quotient_graph(
    graph: &Graph,
    fragments: &[Fragment],
    level: QuotientLevel,
    root: Option<&str>,
) -> QuotientGraph {
    let mut qg = QuotientGraph::new(level);

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
            .edge_categories
            .get(&(src.clone(), dst.clone()))
            .copied()
            .unwrap_or(EdgeCategory::Generic);

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

fn edge_matches_filter(edge: &QuotientEdge, filter: Option<&FxHashSet<EdgeCategory>>) -> bool {
    match filter {
        None => true,
        Some(f) => edge.categories.keys().any(|c| f.contains(c)),
    }
}

pub fn detect_cycles(
    graph: &Graph,
    fragments: &[Fragment],
    level: QuotientLevel,
    root: Option<&str>,
    edge_types: Option<&FxHashSet<EdgeCategory>>,
) -> Vec<Vec<Arc<str>>> {
    let qg = quotient_graph(graph, fragments, level, root);
    let mut node_ids: Vec<Arc<str>> = qg.nodes.keys().cloned().collect();
    node_ids.sort();
    let index_of: FxHashMap<Arc<str>, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect();

    let n = node_ids.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for ((src, dst), edge) in &qg.edges {
        if !edge_matches_filter(edge, edge_types) {
            continue;
        }
        let si = index_of[src];
        let di = index_of[dst];
        adj[si].push(di);
    }

    tarjan_scc(&adj)
        .into_iter()
        .filter(|comp| comp.len() > 1)
        .map(|comp| comp.into_iter().map(|i| node_ids[i].clone()).collect())
        .collect()
}

struct TarjanState {
    index: usize,
    indices: Vec<Option<usize>>,
    lowlinks: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    components: Vec<Vec<usize>>,
}

fn tarjan_scc(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut state = TarjanState {
        index: 0,
        indices: vec![None; n],
        lowlinks: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        components: Vec::new(),
    };
    for v in 0..n {
        if state.indices[v].is_none() {
            strongconnect(v, adj, &mut state);
        }
    }
    state.components
}

fn strongconnect(v: usize, adj: &[Vec<usize>], state: &mut TarjanState) {
    let mut call_stack: Vec<(usize, usize)> = vec![(v, 0)];
    state.indices[v] = Some(state.index);
    state.lowlinks[v] = state.index;
    state.index += 1;
    state.stack.push(v);
    state.on_stack[v] = true;

    while let Some(&(node, iter_pos)) = call_stack.last() {
        if iter_pos < adj[node].len() {
            let w = adj[node][iter_pos];
            if let Some(last) = call_stack.last_mut() {
                last.1 += 1;
            }
            match state.indices[w] {
                None => {
                    state.indices[w] = Some(state.index);
                    state.lowlinks[w] = state.index;
                    state.index += 1;
                    state.stack.push(w);
                    state.on_stack[w] = true;
                    call_stack.push((w, 0));
                }
                Some(w_idx) => {
                    if state.on_stack[w] {
                        let cur = state.lowlinks[node];
                        state.lowlinks[node] = cur.min(w_idx);
                    }
                }
            }
        } else {
            let node_idx =
                state.indices[node].expect("node must have index when popping in tarjan");
            if state.lowlinks[node] == node_idx {
                let mut component = Vec::new();
                while let Some(w) = state.stack.pop() {
                    state.on_stack[w] = false;
                    component.push(w);
                    if w == node {
                        break;
                    }
                }
                state.components.push(component);
            }
            call_stack.pop();
            if let Some(&(parent, _)) = call_stack.last() {
                let combined = state.lowlinks[parent].min(state.lowlinks[node]);
                state.lowlinks[parent] = combined;
            }
        }
    }
}

pub fn coupling_metrics(
    graph: &Graph,
    fragments: &[Fragment],
    level: QuotientLevel,
    root: Option<&str>,
    edge_types: Option<&FxHashSet<EdgeCategory>>,
) -> Vec<ModuleMetrics> {
    let qg = quotient_graph(graph, fragments, level, root);

    let mut out_weight: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    let mut in_weight: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    let mut fan_in_set: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();
    let mut fan_out_set: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();

    for ((src, dst), edge) in &qg.edges {
        if !edge_matches_filter(edge, edge_types) {
            continue;
        }
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
    churn: Option<&FxHashMap<Arc<str>, u32>>,
) -> Vec<HotspotEntry> {
    let mut file_frag_count: FxHashMap<Arc<str>, u32> = FxHashMap::default();
    for frag in fragments {
        let rel: Arc<str> = Arc::from(relative_path(frag.id.path.as_ref(), root));
        *file_frag_count.entry(rel).or_insert(0) += 1;
    }

    let mut out_deg: FxHashMap<Arc<str>, u32> = FxHashMap::default();
    for ((src, _dst), cat) in &graph.edge_categories {
        if let Some(filter) = edge_types
            && !filter.contains(cat)
        {
            continue;
        }
        let rel: Arc<str> = Arc::from(relative_path(src.path.as_ref(), root));
        *out_deg.entry(rel).or_insert(0) += 1;
    }

    let max_deg = out_deg.values().copied().max().unwrap_or(0).max(1);
    let max_churn = churn
        .map_or(0, |c| c.values().copied().max().unwrap_or(0))
        .max(1);

    let mut scored: Vec<HotspotEntry> = file_frag_count
        .into_keys()
        .map(|file| {
            let deg = out_deg.get(&file).copied().unwrap_or(0);
            let ch = churn.and_then(|c| c.get(&file).copied()).unwrap_or(0);
            let deg_norm = f64::from(deg) / f64::from(max_deg);
            let churn_norm = f64::from(ch) / f64::from(max_churn);
            let score = round4(
                ANALYTICS
                    .hotspot_degree_weight
                    .mul_add(deg_norm, ANALYTICS.hotspot_churn_weight * churn_norm),
            );
            HotspotEntry {
                path: file,
                score,
                out_degree: deg,
                churn: ch,
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
            .map_or("?", |(c, _)| category_name(*c));
        let weight_str = format_weight(edge.weight);
        lines.push(format!(
            "    {src_id} -->|\"{top_cat}: {weight_str}\"| {dst_id}"
        ));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn category_name(c: EdgeCategory) -> &'static str {
    match c {
        EdgeCategory::Semantic => "semantic",
        EdgeCategory::Structural => "structural",
        EdgeCategory::Sibling => "sibling",
        EdgeCategory::Config => "config",
        EdgeCategory::ConfigGeneric => "config_generic",
        EdgeCategory::Document => "document",
        EdgeCategory::Similarity => "similarity",
        EdgeCategory::History => "history",
        EdgeCategory::TestEdge => "test_edge",
        EdgeCategory::Generic => "generic",
    }
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

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

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
            g.edge_categories.insert((s.clone(), d.clone()), *c);
        }
        g.freeze();
        g
    }

    #[test]
    fn detect_cycles_finds_simple_loop() {
        let frags = vec![
            frag("pkg/a.rs", 1, 5, 10),
            frag("pkg/b.rs", 1, 5, 10),
            frag("pkg/c.rs", 1, 5, 10),
            frag("pkg/d.rs", 1, 5, 10),
        ];
        let edges = vec![
            (
                frags[0].id.clone(),
                frags[1].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[1].id.clone(),
                frags[2].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[2].id.clone(),
                frags[0].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
            (
                frags[2].id.clone(),
                frags[3].id.clone(),
                1.0,
                EdgeCategory::Semantic,
            ),
        ];
        let g = build(&edges, &frags);
        let cycles = detect_cycles(&g, &frags, QuotientLevel::File, None, None);
        assert_eq!(cycles.len(), 1);
        let c: FxHashSet<&str> = cycles[0].iter().map(|s| s.as_ref()).collect();
        assert!(c.contains("pkg/a.rs"));
        assert!(c.contains("pkg/b.rs"));
        assert!(c.contains("pkg/c.rs"));
        assert!(!c.contains("pkg/d.rs"));
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
        let hs = hotspots(&g, &frags, 2, None, None, None);
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
        let qg = quotient_graph(&g, &frags, QuotientLevel::Directory, None);
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
        let qg = quotient_graph(&g, &frags, QuotientLevel::Directory, None);
        let mermaid = to_mermaid(&qg, 20);
        assert!(mermaid.starts_with("graph LR"));
        assert!(mermaid.contains("dirA"));
        assert!(mermaid.contains("dirB"));
        assert!(mermaid.contains("structural: 3"));
        assert!(mermaid.ends_with('\n'));
    }

    #[test]
    fn mermaid_empty_graph() {
        let qg = QuotientGraph::new(QuotientLevel::Directory);
        assert_eq!(to_mermaid(&qg, 20), "graph LR\n");
    }

    #[test]
    fn detect_cycles_respects_edge_type_filter() {
        let frags = vec![frag("a.rs", 1, 5, 10), frag("b.rs", 1, 5, 10)];
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
                1.0,
                EdgeCategory::History,
            ),
        ];
        let g = build(&edges, &frags);

        let mut filter = FxHashSet::default();
        filter.insert(EdgeCategory::Semantic);
        let cycles = detect_cycles(&g, &frags, QuotientLevel::File, None, Some(&filter));
        assert!(cycles.is_empty());

        let cycles_all = detect_cycles(&g, &frags, QuotientLevel::File, None, None);
        assert_eq!(cycles_all.len(), 1);
    }
}
