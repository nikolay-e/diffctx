use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{Fragment, FragmentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeCategory {
    Semantic,
    Structural,
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
            "config" => Self::Config,
            "config_generic" => Self::ConfigGeneric,
            "document" => Self::Document,
            "similarity" => Self::Similarity,
            "history" => Self::History,
            "test_edge" => Self::TestEdge,
            _ => Self::Generic,
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

pub struct Graph {
    nodes: FxHashSet<FragmentId>,
    fwd: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    rev: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    pub edge_categories: FxHashMap<(FragmentId, FragmentId), EdgeCategory>,
    csr_cache: Option<(CsrGraph, CsrGraph)>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: FxHashSet::default(),
            fwd: FxHashMap::default(),
            rev: FxHashMap::default(),
            edge_categories: FxHashMap::default(),
            csr_cache: None,
        }
    }

    pub fn add_node(&mut self, node: FragmentId) {
        self.nodes.insert(node);
    }

    pub fn add_edge(&mut self, src: FragmentId, dst: FragmentId, weight: f64) {
        if weight.is_nan() || weight.is_infinite() || weight <= 0.0 {
            return;
        }

        let fwd_nbrs = self.fwd.entry(src.clone()).or_default();
        let existing = fwd_nbrs.get(&dst).copied().unwrap_or(0.0);
        let new_weight = existing.max(weight);
        fwd_nbrs.insert(dst.clone(), new_weight);

        let rev_nbrs = self.rev.entry(dst).or_default();
        rev_nbrs.insert(src, new_weight);
    }

    pub fn neighbors(&self, node: &FragmentId) -> Option<&FxHashMap<FragmentId, f64>> {
        self.fwd.get(node)
    }

    pub fn reverse_neighbors(&self, node: &FragmentId) -> Option<&FxHashMap<FragmentId, f64>> {
        self.rev.get(node)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.fwd.values().map(|nbrs| nbrs.len()).sum()
    }

    pub fn to_csr(&mut self) -> &(CsrGraph, CsrGraph) {
        if self.csr_cache.is_some() {
            return self.csr_cache.as_ref().unwrap();
        }

        let mut nodes: Vec<FragmentId> = self.nodes.iter().cloned().collect();
        nodes.sort();

        let node_to_idx: FxHashMap<FragmentId, u32> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as u32))
            .collect();

        let fwd_csr = build_csr(&self.fwd, &nodes, &node_to_idx);
        let rev_csr = build_csr(&self.rev, &nodes, &node_to_idx);

        self.csr_cache = Some((fwd_csr, rev_csr));
        self.csr_cache.as_ref().unwrap()
    }

    pub fn ego_graph(
        &self,
        seeds: &FxHashSet<FragmentId>,
        radius: usize,
    ) -> FxHashMap<FragmentId, f64> {
        if self.nodes.is_empty() {
            return FxHashMap::default();
        }

        let valid_seeds: Vec<&FragmentId> =
            seeds.iter().filter(|s| self.nodes.contains(*s)).collect();

        let per_seed: Vec<FxHashMap<FragmentId, f64>> = valid_seeds
            .par_iter()
            .map(|seed| {
                let visited = self.bfs_from_seed(seed, radius);
                let mut local: FxHashMap<FragmentId, f64> = FxHashMap::default();
                for (node, dist) in visited {
                    let hop_score = if dist > 0 {
                        1.0 / (1 + dist) as f64
                    } else {
                        1.0
                    };
                    local.insert(node, hop_score);
                }
                local
            })
            .collect();

        let mut scores: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for local in per_seed {
            for (node, hop_score) in local {
                let entry = scores.entry(node).or_insert(0.0);
                if hop_score > *entry {
                    *entry = hop_score;
                }
            }
        }

        scores
    }

    fn bfs_from_seed(&self, seed: &FragmentId, radius: usize) -> FxHashMap<FragmentId, usize> {
        let mut visited: FxHashMap<FragmentId, usize> = FxHashMap::default();
        visited.insert(seed.clone(), 0);
        let mut frontier: FxHashMap<FragmentId, usize> = FxHashMap::default();
        frontier.insert(seed.clone(), 0);

        for _step in 0..radius {
            let mut next_frontier: FxHashMap<FragmentId, usize> = FxHashMap::default();
            for (node, dist) in &frontier {
                let new_dist = dist + 1;
                Self::visit_adj(&self.fwd, node, new_dist, &mut visited, &mut next_frontier);
                Self::visit_adj(&self.rev, node, new_dist, &mut visited, &mut next_frontier);
            }
            frontier = next_frontier;
        }

        visited
    }

    fn visit_adj(
        adj: &FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
        node: &FragmentId,
        new_dist: usize,
        visited: &mut FxHashMap<FragmentId, usize>,
        next_frontier: &mut FxHashMap<FragmentId, usize>,
    ) {
        if let Some(nbrs) = adj.get(node) {
            for nbr in nbrs.keys() {
                if !visited.contains_key(nbr) {
                    visited.insert(nbr.clone(), new_dist);
                    next_frontier.insert(nbr.clone(), new_dist);
                }
            }
        }
    }
}

fn build_csr(
    adj: &FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
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

const HUB_OUT_DEGREE_THRESHOLD: usize = 3;

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
    let d_median = if degrees_sorted.is_empty() {
        0.0
    } else {
        let mid = degrees_sorted.len() / 2;
        if degrees_sorted.len() % 2 == 0 {
            (degrees_sorted[mid - 1] as f64 + degrees_sorted[mid] as f64) / 2.0
        } else {
            degrees_sorted[mid] as f64
        }
    };

    for i in 0..edge_list.len() {
        let dst_deg = in_degree[dst_indices[i] as usize] as f64;
        if dst_deg > d_median && !is_exempt[i] {
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
                if src_deg >= HUB_OUT_DEGREE_THRESHOLD as u32 {
                    edge_weights[i] /= (src_deg as f64).sqrt();
                }
            }
        }
    }

    for (idx, ((src, dst), _)) in edge_list.iter().enumerate() {
        edges.insert((src.clone(), dst.clone()), edge_weights[idx]);
    }
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

    for ((src, dst), w) in &edges {
        if *w > 0.0 {
            graph
                .fwd
                .entry(src.clone())
                .or_default()
                .insert(dst.clone(), *w);
            graph
                .rev
                .entry(dst.clone())
                .or_default()
                .insert(src.clone(), *w);
        }
    }

    graph.edge_categories = categories;
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fid(path: &str, start: u32, end: u32) -> FragmentId {
        FragmentId::new(Arc::from(path), start, end)
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

        assert_eq!(g.neighbors(&a).unwrap()[&b], 0.8);
        assert_eq!(g.reverse_neighbors(&b).unwrap()[&a], 0.8);
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

        assert!(g.neighbors(&a).is_none());
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

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let scores = g.ego_graph(&seeds, 2);

        assert_eq!(scores[&a], 1.0);
        assert!((scores[&b] - 0.5).abs() < 1e-9);
        assert!((scores[&c] - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ego_graph_empty() {
        let g = Graph::new();
        let seeds = FxHashSet::default();
        let scores = g.ego_graph(&seeds, 2);
        assert!(scores.is_empty());
    }
}
