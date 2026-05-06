use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::config::scoring::EGO;
use crate::types::{Fragment, FragmentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeCategory {
    Semantic,
    Structural,
    Sibling,
    Config,
    ConfigGeneric,
    Document,
    Similarity,
    History,
    TestEdge,
    Generic,
}

impl EdgeCategory {
    pub fn from_str(s: &str) -> Self {
        match s {
            "semantic" => Self::Semantic,
            "structural" => Self::Structural,
            "sibling" => Self::Sibling,
            "config" => Self::Config,
            "config_generic" => Self::ConfigGeneric,
            "document" => Self::Document,
            "similarity" => Self::Similarity,
            "history" => Self::History,
            "test_edge" => Self::TestEdge,
            _ => Self::Generic,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Structural => "structural",
            Self::Sibling => "sibling",
            Self::Config => "config",
            Self::ConfigGeneric => "config_generic",
            Self::Document => "document",
            Self::Similarity => "similarity",
            Self::History => "history",
            Self::TestEdge => "test_edge",
            Self::Generic => "generic",
        }
    }

    fn is_suppression_exempt(self) -> bool {
        matches!(self, Self::Semantic | Self::Structural | Self::TestEdge)
    }
}

pub struct CsrGraph {
    pub n: usize,
    pub indptr: Vec<u32>,
    pub indices: Vec<u32>,
    pub weights: Vec<f64>,
    pub out_weight_sum: Vec<f64>,
    pub node_to_idx: FxHashMap<FragmentId, u32>,
    pub idx_to_node: Vec<FragmentId>,
}

/// Statistics from the per-source out-edge cap that runs in
/// `build_graph` after `apply_hub_suppression`. Surfaced to Python
/// via `LatencyBreakdown` so calibration runs can quantify how often
/// the cap fires and how many edges it discards.
#[derive(Default, Clone, Copy)]
pub struct EdgeCapStats {
    /// Edge count after merge + hub suppression, before cap.
    pub edges_before_cap: usize,
    /// Edge count after cap.
    pub edges_after_cap: usize,
    /// Edges discarded by the cap (lowest-weight neighbors of overfull nodes).
    pub edges_dropped_by_cap: usize,
    /// Number of source nodes that had > `max_per_node` outgoing edges
    /// and therefore had their neighbor list truncated.
    pub nodes_capped: usize,
    /// The `max_per_node` value actually applied (after env-var override).
    pub max_out_edges_per_node: usize,
}

pub struct Graph {
    nodes: FxHashSet<FragmentId>,
    fwd: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    rev: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    pub edge_categories: FxHashMap<(FragmentId, FragmentId), EdgeCategory>,
    csr_cache: Option<(CsrGraph, CsrGraph)>,
    pub cap_stats: EdgeCapStats,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: FxHashSet::default(),
            fwd: FxHashMap::default(),
            rev: FxHashMap::default(),
            edge_categories: FxHashMap::default(),
            csr_cache: None,
            cap_stats: EdgeCapStats::default(),
        }
    }

    pub fn add_node(&mut self, node: FragmentId) {
        self.nodes.insert(node);
    }

    pub fn add_edge(&mut self, src: FragmentId, dst: FragmentId, weight: f64) {
        if weight.is_nan() || weight.is_infinite() || weight <= 0.0 {
            return;
        }
        debug_assert!(
            self.csr_cache.is_none(),
            "add_edge called after Graph was frozen"
        );

        let fwd_nbrs = self.fwd.entry(src.clone()).or_default();
        let existing = fwd_nbrs.get(&dst).copied().unwrap_or(0.0);
        let new_weight = existing.max(weight);
        fwd_nbrs.insert(dst.clone(), new_weight);

        let rev_nbrs = self.rev.entry(dst).or_default();
        rev_nbrs.insert(src, new_weight);
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &FragmentId> {
        self.nodes.iter()
    }

    pub fn edge_count(&self) -> usize {
        if let Some((fwd, _)) = &self.csr_cache {
            return fwd.indices.len();
        }
        self.fwd.values().map(|nbrs| nbrs.len()).sum()
    }

    /// Convert the build-time hashmap representation into CSR and drop the hashmaps.
    /// After freeze, fwd/rev are empty and all reads go through CSR.
    pub fn freeze(&mut self) {
        if self.csr_cache.is_some() {
            return;
        }

        let mut nodes: Vec<FragmentId> = self.nodes.iter().cloned().collect();
        nodes.sort();

        let node_to_idx: FxHashMap<FragmentId, u32> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as u32))
            .collect();

        let fwd = std::mem::take(&mut self.fwd);
        let rev = std::mem::take(&mut self.rev);

        let fwd_csr = build_csr_owned(fwd, &nodes, &node_to_idx);
        let rev_csr = build_csr_owned(rev, &nodes, &node_to_idx);

        self.csr_cache = Some((fwd_csr, rev_csr));
    }

    pub fn to_csr(&mut self) -> &(CsrGraph, CsrGraph) {
        self.freeze();
        self.csr_cache.as_ref().unwrap()
    }

    pub fn fwd_csr(&self) -> Option<&CsrGraph> {
        self.csr_cache.as_ref().map(|(f, _)| f)
    }

    pub fn rev_csr(&self) -> Option<&CsrGraph> {
        self.csr_cache.as_ref().map(|(_, r)| r)
    }

    /// Look up the weight of edge `src -> dst` in the forward CSR.
    pub fn forward_edge_weight(&self, src: &FragmentId, dst: &FragmentId) -> Option<f64> {
        let fwd = self.fwd_csr()?;
        let src_idx = *fwd.node_to_idx.get(src)? as usize;
        let dst_idx = *fwd.node_to_idx.get(dst)?;
        let s = fwd.indptr[src_idx] as usize;
        let e = fwd.indptr[src_idx + 1] as usize;
        for k in s..e {
            if fwd.indices[k] == dst_idx {
                return Some(fwd.weights[k]);
            }
        }
        None
    }

    /// Invoke `f(neighbor_id, weight)` for each forward neighbor of `node`.
    pub fn for_each_forward_neighbor<F: FnMut(&FragmentId, f64)>(
        &self,
        node: &FragmentId,
        mut f: F,
    ) {
        let fwd = match self.fwd_csr() {
            Some(c) => c,
            None => return,
        };
        let idx = match fwd.node_to_idx.get(node) {
            Some(&i) => i as usize,
            None => return,
        };
        let s = fwd.indptr[idx] as usize;
        let e = fwd.indptr[idx + 1] as usize;
        for k in s..e {
            let dst_idx = fwd.indices[k] as usize;
            f(&fwd.idx_to_node[dst_idx], fwd.weights[k]);
        }
    }

    pub fn ego_graph(
        &self,
        seeds: &FxHashSet<FragmentId>,
        radius: usize,
    ) -> FxHashMap<FragmentId, f64> {
        let (fwd, rev) = match &self.csr_cache {
            Some(c) => c,
            None => return FxHashMap::default(),
        };
        if fwd.n == 0 {
            return FxHashMap::default();
        }

        let valid_seed_idxs: Vec<u32> = seeds
            .iter()
            .filter_map(|s| fwd.node_to_idx.get(s).copied())
            .collect();

        let per_seed: Vec<Vec<(u32, u32, f64)>> = valid_seed_idxs
            .par_iter()
            .map(|&seed_idx| bfs_from_seed_with_path_weight(fwd, rev, seed_idx, radius))
            .collect();

        let gamma = EGO.per_hop_decay;
        let mut scores: FxHashMap<u32, f64> = FxHashMap::default();
        for visits in per_seed {
            for (idx, dist, w_path) in visits {
                let contribution = gamma.powi(dist as i32) * w_path;
                *scores.entry(idx).or_insert(0.0) += contribution;
            }
        }

        scores
            .into_iter()
            .map(|(idx, score)| (fwd.idx_to_node[idx as usize].clone(), score))
            .collect()
    }
}

/// BFS over `fwd ∪ rev` from a single seed, tracking both the shortest
/// hop distance and the max-product edge-weight path of that length.
///
/// Implements the paper's `R_ego` kernel (§4.4.2):
/// `R_ego(v) = Σ_{u∈E_0} 1[d_hop(u,v) ≤ L] · γ^{d_hop} · W_path(u,v)`
/// where `W_path(u,v) = max_π ∏_{(a,b)∈π} w_{ab}` over paths of length
/// equal to `d_hop`. The `Σ` over seeds is performed in `ego_graph`;
/// per-seed shortest-distance + max-product is computed here.
fn bfs_from_seed_with_path_weight(
    fwd: &CsrGraph,
    rev: &CsrGraph,
    seed_idx: u32,
    radius: usize,
) -> Vec<(u32, u32, f64)> {
    let n = fwd.n;
    let mut dist = vec![u32::MAX; n];
    let mut max_w = vec![0.0_f64; n];
    dist[seed_idx as usize] = 0;
    max_w[seed_idx as usize] = 1.0;
    let mut frontier: Vec<u32> = vec![seed_idx];

    for step in 0..radius {
        let new_dist = (step + 1) as u32;
        let mut next: Vec<u32> = Vec::new();
        for &u in &frontier {
            let ui = u as usize;
            let w_u = max_w[ui];
            for csr in [fwd, rev] {
                let s = csr.indptr[ui] as usize;
                let e = csr.indptr[ui + 1] as usize;
                for k in s..e {
                    let v = csr.indices[k];
                    let w_uv = csr.weights[k];
                    let candidate = w_u * w_uv;
                    let vi = v as usize;
                    if dist[vi] == u32::MAX {
                        dist[vi] = new_dist;
                        max_w[vi] = candidate;
                        next.push(v);
                    } else if dist[vi] == new_dist && candidate > max_w[vi] {
                        max_w[vi] = candidate;
                    }
                }
            }
        }
        frontier = next;
    }

    let mut result = Vec::new();
    for i in 0..n {
        if dist[i] != u32::MAX {
            result.push((i as u32, dist[i], max_w[i]));
        }
    }
    result
}

fn build_csr_owned(
    adj: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    nodes: &[FragmentId],
    node_to_idx: &FxHashMap<FragmentId, u32>,
) -> CsrGraph {
    let n = nodes.len();
    let total_edges: usize = adj.values().map(|v| v.len()).sum();

    let mut indptr = vec![0u32; n + 1];
    let mut indices = Vec::with_capacity(total_edges);
    let mut weights = Vec::with_capacity(total_edges);

    for (i, node) in nodes.iter().enumerate() {
        if let Some(nbrs) = adj.get(node) {
            let mut edges: Vec<(u32, f64)> = nbrs
                .iter()
                .filter_map(|(dst, &w)| node_to_idx.get(dst).map(|&idx| (idx, w)))
                .collect();
            edges.sort_by_key(|&(idx, _)| idx);
            for (idx, w) in edges {
                indices.push(idx);
                weights.push(w);
            }
        }
        indptr[i + 1] = indices.len() as u32;
    }

    let mut out_weight_sum = vec![0.0f64; n];
    for i in 0..n {
        let s = indptr[i] as usize;
        let e = indptr[i + 1] as usize;
        if e > s {
            out_weight_sum[i] = weights[s..e].iter().sum();
        }
    }

    CsrGraph {
        n,
        indptr,
        indices,
        weights,
        out_weight_sum,
        node_to_idx: node_to_idx.clone(),
        idx_to_node: nodes.to_vec(),
    }
}

fn apply_hub_suppression(
    edges: &mut FxHashMap<(FragmentId, FragmentId), f64>,
    edge_categories: &FxHashMap<(FragmentId, FragmentId), EdgeCategory>,
) {
    if edges.is_empty() {
        return;
    }

    let edge_list: Vec<((FragmentId, FragmentId), f64)> =
        edges.iter().map(|(k, &v)| (k.clone(), v)).collect();

    let mut node_set: FxHashMap<FragmentId, u32> = FxHashMap::default();
    for ((src, dst), _) in &edge_list {
        let len = node_set.len() as u32;
        node_set.entry(src.clone()).or_insert(len);
        let len = node_set.len() as u32;
        node_set.entry(dst.clone()).or_insert(len);
    }
    let n_nodes = node_set.len();

    let mut src_indices: Vec<u32> = Vec::with_capacity(edge_list.len());
    let mut dst_indices: Vec<u32> = Vec::with_capacity(edge_list.len());
    let mut edge_weights: Vec<f64> = Vec::with_capacity(edge_list.len());
    let mut is_semantic: Vec<bool> = Vec::with_capacity(edge_list.len());
    let mut is_exempt: Vec<bool> = Vec::with_capacity(edge_list.len());

    for ((src, dst), w) in &edge_list {
        src_indices.push(node_set[src]);
        dst_indices.push(node_set[dst]);
        edge_weights.push(*w);
        let cat = edge_categories
            .get(&(src.clone(), dst.clone()))
            .copied()
            .unwrap_or(EdgeCategory::Generic);
        is_semantic.push(cat == EdgeCategory::Semantic);
        is_exempt.push(cat.is_suppression_exempt());
    }

    let mut in_degree = vec![0u32; n_nodes];
    for &di in &dst_indices {
        in_degree[di as usize] += 1;
    }

    let mut degrees_sorted: Vec<u32> = in_degree.iter().copied().filter(|&d| d > 0).collect();
    degrees_sorted.sort_unstable();
    let d_p95 = if degrees_sorted.is_empty() {
        0.0
    } else {
        let n = degrees_sorted.len();
        let idx = ((n as f64 * 0.95).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        degrees_sorted[idx] as f64
    };

    for i in 0..edge_list.len() {
        let dst_deg = in_degree[dst_indices[i] as usize] as f64;
        if dst_deg > d_p95 && !is_exempt[i] {
            let divisor = dst_deg.ln_1p().max(1.0);
            edge_weights[i] /= divisor;
        }
    }

    let mut sem_out_files: FxHashMap<u32, FxHashSet<&str>> = FxHashMap::default();
    for (i, ((_, dst), _)) in edge_list.iter().enumerate() {
        if is_semantic[i] {
            sem_out_files
                .entry(src_indices[i])
                .or_default()
                .insert(dst.path.as_ref());
        }
    }

    if !sem_out_files.is_empty() {
        let mut sem_file_deg: Vec<u32> = vec![0; n_nodes];
        for (&si, files) in &sem_out_files {
            sem_file_deg[si as usize] = files.len() as u32;
        }

        for i in 0..edge_list.len() {
            if is_semantic[i] {
                let src_deg = sem_file_deg[src_indices[i] as usize];
                if src_deg >= GRAPH_FILTERING.hub_out_degree_threshold as u32 {
                    edge_weights[i] /= (src_deg as f64).sqrt();
                }
            }
        }
    }

    for (idx, ((src, dst), _)) in edge_list.iter().enumerate() {
        edges.insert((src.clone(), dst.clone()), edge_weights[idx]);
    }
}

/// Default top-K out-edges per source node. Calibrated against the
/// observed edge density distribution: typical Python file emits
/// 5-20 semantic + 2-5 structural + ≤20 sibling edges (~30-50 normal),
/// so K=64 preserves all legitimate edges while clamping pathological
/// dense nodes (e.g. utility hubs in django/material-ui that radiate
/// into thousands of dependents).
const DEFAULT_MAX_OUT_EDGES_PER_NODE: usize = 64;

/// Truncate each node's outgoing edge list to the top-K by weight.
/// Run AFTER `apply_hub_suppression` so the suppression pass sees
/// the true in-degree distribution; otherwise its IDF damping is
/// computed against a graph that has already been thinned.
///
/// Returns the cap stats for diagnostic surfacing into `LatencyBreakdown`.
fn cap_out_edges_per_source(
    edges: &mut FxHashMap<(FragmentId, FragmentId), f64>,
    max_per_node: usize,
) -> EdgeCapStats {
    let edges_before = edges.len();

    let mut by_src: FxHashMap<FragmentId, Vec<(FragmentId, f64)>> = FxHashMap::default();
    for ((src, dst), w) in edges.drain() {
        by_src.entry(src).or_default().push((dst, w));
    }

    let mut nodes_capped = 0;
    let mut edges_dropped = 0;
    for (src, mut neighbors) in by_src {
        if neighbors.len() > max_per_node {
            nodes_capped += 1;
            edges_dropped += neighbors.len() - max_per_node;
            neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            neighbors.truncate(max_per_node);
        }
        for (dst, w) in neighbors {
            edges.insert((src.clone(), dst), w);
        }
    }

    EdgeCapStats {
        edges_before_cap: edges_before,
        edges_after_cap: edges.len(),
        edges_dropped_by_cap: edges_dropped,
        nodes_capped,
        max_out_edges_per_node: max_per_node,
    }
}

fn read_max_out_edges_per_node() -> usize {
    std::env::var("DIFFCTX_MAX_EDGES_PER_NODE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_OUT_EDGES_PER_NODE)
}

pub fn build_graph(
    fragments: &[Fragment],
    mut edges: FxHashMap<(FragmentId, FragmentId), f64>,
    categories: FxHashMap<(FragmentId, FragmentId), EdgeCategory>,
) -> Graph {
    let mut graph = Graph::new();
    for frag in fragments {
        graph.add_node(frag.id.clone());
    }

    apply_hub_suppression(&mut edges, &categories);

    let max_per_node = read_max_out_edges_per_node();
    let cap_stats = cap_out_edges_per_source(&mut edges, max_per_node);
    tracing::debug!(
        "edge cap K={}: {} -> {} (dropped {} from {} nodes)",
        max_per_node,
        cap_stats.edges_before_cap,
        cap_stats.edges_after_cap,
        cap_stats.edges_dropped_by_cap,
        cap_stats.nodes_capped,
    );

    for ((src, dst), w) in edges {
        if w > 0.0 {
            graph
                .fwd
                .entry(src.clone())
                .or_default()
                .insert(dst.clone(), w);
            graph.rev.entry(dst).or_default().insert(src, w);
        }
    }

    graph.edge_categories = categories;
    graph.cap_stats = cap_stats;
    graph.freeze();
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fid(path: &str, start: u32, end: u32) -> FragmentId {
        FragmentId::new(Arc::from(path), start, end)
    }

    fn collect_forward(g: &Graph, node: &FragmentId) -> Vec<(FragmentId, f64)> {
        let mut out = Vec::new();
        g.for_each_forward_neighbor(node, |nbr, w| out.push((nbr.clone(), w)));
        out
    }

    #[test]
    fn add_edge_takes_max_weight() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_edge(a.clone(), b.clone(), 0.5);
        g.add_edge(a.clone(), b.clone(), 0.8);
        g.add_edge(a.clone(), b.clone(), 0.3);
        g.freeze();

        let fwd = collect_forward(&g, &a);
        assert_eq!(fwd.len(), 1);
        assert!((fwd[0].1 - 0.8).abs() < 1e-9);
        assert_eq!(fwd[0].0, b);
    }

    #[test]
    fn add_edge_drops_invalid_weights() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_edge(a.clone(), b.clone(), f64::NAN);
        g.add_edge(a.clone(), b.clone(), f64::INFINITY);
        g.add_edge(a.clone(), b.clone(), -1.0);
        g.add_edge(a.clone(), b.clone(), 0.0);
        g.freeze();

        assert!(collect_forward(&g, &a).is_empty());
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn csr_round_trip() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a.clone(), b.clone(), 1.0);
        g.add_edge(b.clone(), c.clone(), 2.0);

        let (fwd, _rev) = g.to_csr();
        assert_eq!(fwd.n, 3);
        assert_eq!(fwd.indptr.len(), 4);
        assert!(fwd.out_weight_sum[fwd.node_to_idx[&a] as usize] > 0.0);
    }

    #[test]
    fn ego_graph_scores() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a.clone(), b.clone(), 1.0);
        g.add_edge(b.clone(), c.clone(), 1.0);
        g.freeze();

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let scores = g.ego_graph(&seeds, 2);

        let gamma = crate::config::scoring::EGO.per_hop_decay;
        assert!((scores[&a] - 1.0).abs() < 1e-9);
        assert!((scores[&b] - gamma).abs() < 1e-9);
        assert!((scores[&c] - gamma * gamma).abs() < 1e-9);
    }

    #[test]
    fn ego_graph_sums_over_seeds() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let v = fid("v.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(v.clone());
        g.add_edge(a.clone(), v.clone(), 1.0);
        g.add_edge(b.clone(), v.clone(), 1.0);
        g.freeze();

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        seeds.insert(b.clone());
        let scores = g.ego_graph(&seeds, 1);

        let gamma = crate::config::scoring::EGO.per_hop_decay;
        assert!(
            (scores[&v] - 2.0 * gamma).abs() < 1e-9,
            "v reached by 2 seeds at d=1 must score 2·γ; got {}",
            scores[&v]
        );
    }

    #[test]
    fn ego_graph_uses_path_weight() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a.clone(), b.clone(), 0.7);
        g.add_edge(b.clone(), c.clone(), 0.4);
        g.freeze();

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let scores = g.ego_graph(&seeds, 2);

        let gamma = crate::config::scoring::EGO.per_hop_decay;
        assert!(
            (scores[&b] - gamma * 0.7).abs() < 1e-9,
            "1-hop weighted score = γ·0.7; got {}",
            scores[&b]
        );
        assert!(
            (scores[&c] - gamma * gamma * 0.7 * 0.4).abs() < 1e-9,
            "2-hop product-of-weights score = γ²·0.7·0.4; got {}",
            scores[&c]
        );
    }

    #[test]
    fn ego_graph_empty() {
        let mut g = Graph::new();
        g.freeze();
        let seeds = FxHashSet::default();
        let scores = g.ego_graph(&seeds, 2);
        assert!(scores.is_empty());
    }
}
