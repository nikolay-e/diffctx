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

/// A single node-interned edge. 24 bytes instead of a string-keyed
/// hashmap entry — the difference between ~400 MB and several GB on
/// multi-million-edge repositories (vendored Go monorepos, vscode),
/// which is what used to OOM-kill memory-limited benchmark runners.
#[derive(Clone, Copy)]
pub struct CompactEdge {
    pub src: u32,
    pub dst: u32,
    pub weight: f64,
    pub category: EdgeCategory,
}

/// Node-interned edge list: the memory-bounded intermediate between
/// edge collection and graph construction. `idx_to_node` is the sorted,
/// deduplicated fragment-id universe, so CSR node indexing built from it
/// is identical to the ordering `Graph::freeze` produces.
pub struct CompactEdges {
    pub node_to_idx: FxHashMap<FragmentId, u32>,
    pub idx_to_node: Vec<FragmentId>,
    pub edges: Vec<CompactEdge>,
}

pub fn intern_fragment_nodes(
    fragments: &[Fragment],
) -> (FxHashMap<FragmentId, u32>, Vec<FragmentId>) {
    let mut idx_to_node: Vec<FragmentId> = fragments.iter().map(|f| f.id.clone()).collect();
    idx_to_node.sort();
    idx_to_node.dedup();
    let node_to_idx = idx_to_node
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u32))
        .collect();
    (node_to_idx, idx_to_node)
}

/// Merge duplicate (src, dst) entries: weight = max across duplicates,
/// category = first occurrence in input order (builder order), matching
/// the historical `EdgeDict` max-merge + `or_insert` category semantics.
/// Requires a stable sort so first-in-input stays first-in-group.
pub fn dedup_compact_edges(edges: &mut Vec<CompactEdge>) {
    edges.sort_by(|a, b| (a.src, a.dst).cmp(&(b.src, b.dst)));
    let mut out = 0usize;
    let mut i = 0usize;
    while i < edges.len() {
        let mut merged = edges[i];
        let mut j = i + 1;
        while j < edges.len() && edges[j].src == merged.src && edges[j].dst == merged.dst {
            if edges[j].weight > merged.weight {
                merged.weight = edges[j].weight;
            }
            j += 1;
        }
        edges[out] = merged;
        out += 1;
        i = j;
    }
    edges.truncate(out);
}

/// Compact (src, dst) -> category lookup over the pre-cap edge set.
/// Replaces the FragmentId-pair-keyed hashmap that used to retain the
/// full uncapped edge universe for the lifetime of the pipeline.
#[derive(Default)]
pub struct EdgeCategoryTable {
    node_to_idx: FxHashMap<FragmentId, u32>,
    idx_to_node: Vec<FragmentId>,
    entries: Vec<(u32, u32, EdgeCategory)>,
    sorted: bool,
}

impl EdgeCategoryTable {
    fn from_sorted_parts(
        node_to_idx: FxHashMap<FragmentId, u32>,
        idx_to_node: Vec<FragmentId>,
        entries: Vec<(u32, u32, EdgeCategory)>,
    ) -> Self {
        debug_assert!(
            entries
                .windows(2)
                .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1))
        );
        Self {
            node_to_idx,
            idx_to_node,
            entries,
            sorted: true,
        }
    }

    fn intern(&mut self, id: FragmentId) -> u32 {
        if let Some(&i) = self.node_to_idx.get(&id) {
            return i;
        }
        let i = self.idx_to_node.len() as u32;
        self.idx_to_node.push(id.clone());
        self.node_to_idx.insert(id, i);
        i
    }

    pub fn insert(&mut self, src: FragmentId, dst: FragmentId, category: EdgeCategory) {
        let s = self.intern(src);
        let d = self.intern(dst);
        self.entries.push((s, d, category));
        self.sorted = false;
    }

    /// Stable-sort + last-wins dedup, matching hashmap overwrite
    /// semantics for repeated `insert` of the same key.
    pub fn ensure_sorted(&mut self) {
        if self.sorted {
            return;
        }
        self.entries.sort_by_key(|e| (e.0, e.1));
        let mut out = 0usize;
        let mut i = 0usize;
        while i < self.entries.len() {
            let mut j = i;
            while j + 1 < self.entries.len()
                && self.entries[j + 1].0 == self.entries[i].0
                && self.entries[j + 1].1 == self.entries[i].1
            {
                j += 1;
            }
            self.entries[out] = self.entries[j];
            out += 1;
            i = j + 1;
        }
        self.entries.truncate(out);
        self.sorted = true;
    }

    pub fn get(&self, src: &FragmentId, dst: &FragmentId) -> Option<EdgeCategory> {
        debug_assert!(self.sorted, "EdgeCategoryTable queried before freeze");
        let s = *self.node_to_idx.get(src)?;
        let d = *self.node_to_idx.get(dst)?;
        self.entries
            .binary_search_by_key(&(s, d), |e| (e.0, e.1))
            .ok()
            .map(|k| self.entries[k].2)
    }

    pub fn for_each<F: FnMut(&FragmentId, &FragmentId, EdgeCategory)>(&self, mut f: F) {
        for &(s, d, c) in &self.entries {
            f(
                &self.idx_to_node[s as usize],
                &self.idx_to_node[d as usize],
                c,
            );
        }
    }
}

pub struct Graph {
    nodes: FxHashSet<FragmentId>,
    fwd: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    rev: FxHashMap<FragmentId, FxHashMap<FragmentId, f64>>,
    edge_categories: EdgeCategoryTable,
    csr_cache: Option<(CsrGraph, CsrGraph)>,
    pub cap_stats: EdgeCapStats,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: FxHashSet::default(),
            fwd: FxHashMap::default(),
            rev: FxHashMap::default(),
            edge_categories: EdgeCategoryTable::default(),
            csr_cache: None,
            cap_stats: EdgeCapStats::default(),
        }
    }

    pub fn edge_category(&self, src: &FragmentId, dst: &FragmentId) -> Option<EdgeCategory> {
        self.edge_categories.get(src, dst)
    }

    pub fn for_each_categorized_edge<F: FnMut(&FragmentId, &FragmentId, EdgeCategory)>(
        &self,
        f: F,
    ) {
        self.edge_categories.for_each(f)
    }

    pub fn insert_edge_category(&mut self, src: FragmentId, dst: FragmentId, cat: EdgeCategory) {
        self.edge_categories.insert(src, dst, cat);
    }

    pub fn categorized_edge_count(&self) -> usize {
        self.edge_categories.entries.len()
    }

    pub fn add_node(&mut self, node: FragmentId) {
        self.nodes.insert(node);
    }

    pub fn add_edge(&mut self, src: FragmentId, dst: FragmentId, weight: f64) {
        if weight.is_nan() || weight.is_infinite() || weight <= 0.0 {
            return;
        }
        if src == dst {
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
        self.edge_categories.ensure_sorted();
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

        let mut valid_seed_idxs: Vec<u32> = seeds
            .iter()
            .filter_map(|s| fwd.node_to_idx.get(s).copied())
            .collect();
        valid_seed_idxs.sort_unstable();

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

fn apply_hub_suppression(edges: &mut [CompactEdge], idx_to_node: &[FragmentId]) {
    if edges.is_empty() {
        return;
    }
    let n_nodes = idx_to_node.len();

    let mut in_degree = vec![0u32; n_nodes];
    for e in edges.iter() {
        in_degree[e.dst as usize] += 1;
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

    for e in edges.iter_mut() {
        let dst_deg = in_degree[e.dst as usize] as f64;
        if dst_deg > d_p95 && !e.category.is_suppression_exempt() {
            let divisor = dst_deg.ln_1p().max(1.0);
            e.weight /= divisor;
        }
    }

    let mut sem_out_files: FxHashMap<u32, FxHashSet<&str>> = FxHashMap::default();
    for e in edges.iter() {
        if e.category == EdgeCategory::Semantic {
            sem_out_files
                .entry(e.src)
                .or_default()
                .insert(idx_to_node[e.dst as usize].path.as_ref());
        }
    }

    if !sem_out_files.is_empty() {
        let mut sem_file_deg: Vec<u32> = vec![0; n_nodes];
        for (&si, files) in &sem_out_files {
            sem_file_deg[si as usize] = files.len() as u32;
        }

        for e in edges.iter_mut() {
            if e.category == EdgeCategory::Semantic {
                let src_deg = sem_file_deg[e.src as usize];
                if src_deg >= GRAPH_FILTERING.hub_out_degree_threshold as u32 {
                    e.weight /= (src_deg as f64).sqrt();
                }
            }
        }
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
/// Keeps each source's top-K neighbors by weight (ties broken by dst
/// index for determinism), truncating the rest in place.
fn cap_out_edges_per_source(edges: &mut Vec<CompactEdge>, max_per_node: usize) -> EdgeCapStats {
    let edges_before = edges.len();

    edges.sort_unstable_by(|a, b| {
        a.src
            .cmp(&b.src)
            .then_with(|| {
                b.weight
                    .partial_cmp(&a.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.dst.cmp(&b.dst))
    });

    let mut nodes_capped = 0;
    let mut out = 0usize;
    let mut i = 0usize;
    while i < edges.len() {
        let src = edges[i].src;
        let mut j = i;
        while j < edges.len() && edges[j].src == src {
            j += 1;
        }
        let group = j - i;
        if group > max_per_node {
            nodes_capped += 1;
        }
        let take = group.min(max_per_node);
        for k in i..i + take {
            edges[out] = edges[k];
            out += 1;
        }
        i = j;
    }
    edges.truncate(out);

    EdgeCapStats {
        edges_before_cap: edges_before,
        edges_after_cap: edges.len(),
        edges_dropped_by_cap: edges_before - edges.len(),
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

/// Build a CSR from (src, dst, weight) triples over the interned node
/// universe. Sorting by (src, dst) reproduces the neighbor ordering the
/// hashmap-based `freeze` path produced (neighbors sorted by dst index),
/// so weights, out-weight sums, and traversal results are bit-identical.
fn build_csr_from_pairs(
    mut pairs: Vec<(u32, u32, f64)>,
    idx_to_node: &[FragmentId],
    node_to_idx: &FxHashMap<FragmentId, u32>,
) -> CsrGraph {
    pairs.sort_unstable_by_key(|p| (p.0, p.1));
    let n = idx_to_node.len();

    let mut indptr = vec![0u32; n + 1];
    let mut indices = Vec::with_capacity(pairs.len());
    let mut weights = Vec::with_capacity(pairs.len());

    let mut row = 0usize;
    for &(src, dst, w) in &pairs {
        while row < src as usize {
            row += 1;
            indptr[row] = indices.len() as u32;
        }
        indices.push(dst);
        weights.push(w);
    }
    while row < n {
        row += 1;
        indptr[row] = indices.len() as u32;
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
        idx_to_node: idx_to_node.to_vec(),
    }
}

/// Main graph-construction path: hub suppression, per-source cap, and
/// CSR assembly all run on the interned edge array, never materializing
/// FragmentId-keyed maps of the full edge universe.
pub fn build_graph_compact(fragments: &[Fragment], compact: CompactEdges) -> Graph {
    let CompactEdges {
        node_to_idx,
        idx_to_node,
        mut edges,
    } = compact;

    apply_hub_suppression(&mut edges, &idx_to_node);

    let category_entries: Vec<(u32, u32, EdgeCategory)> =
        edges.iter().map(|e| (e.src, e.dst, e.category)).collect();

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

    let fwd_pairs: Vec<(u32, u32, f64)> = edges
        .iter()
        .filter(|e| e.weight > 0.0)
        .map(|e| (e.src, e.dst, e.weight))
        .collect();
    let rev_pairs: Vec<(u32, u32, f64)> = edges
        .iter()
        .filter(|e| e.weight > 0.0)
        .map(|e| (e.dst, e.src, e.weight))
        .collect();
    drop(edges);

    let fwd_csr = build_csr_from_pairs(fwd_pairs, &idx_to_node, &node_to_idx);
    let rev_csr = build_csr_from_pairs(rev_pairs, &idx_to_node, &node_to_idx);

    let mut graph = Graph::new();
    for frag in fragments {
        graph.nodes.insert(frag.id.clone());
    }
    graph.edge_categories =
        EdgeCategoryTable::from_sorted_parts(node_to_idx, idx_to_node, category_entries);
    graph.cap_stats = cap_stats;
    graph.csr_cache = Some((fwd_csr, rev_csr));
    graph
}

/// Map-based adapter kept for tests and small callers; converts to the
/// compact representation and delegates. Edges whose endpoints are not
/// in `fragments` are dropped (the hashmap path silently dropped them
/// at CSR construction).
pub fn build_graph(
    fragments: &[Fragment],
    edges: FxHashMap<(FragmentId, FragmentId), f64>,
    categories: FxHashMap<(FragmentId, FragmentId), EdgeCategory>,
) -> Graph {
    let (node_to_idx, idx_to_node) = intern_fragment_nodes(fragments);
    let mut compact_edges = Vec::with_capacity(edges.len());
    for ((src, dst), w) in &edges {
        let s = match node_to_idx.get(src) {
            Some(&i) => i,
            None => continue,
        };
        let d = match node_to_idx.get(dst) {
            Some(&i) => i,
            None => continue,
        };
        let category = categories
            .get(&(src.clone(), dst.clone()))
            .copied()
            .unwrap_or(EdgeCategory::Generic);
        compact_edges.push(CompactEdge {
            src: s,
            dst: d,
            weight: *w,
            category,
        });
    }
    dedup_compact_edges(&mut compact_edges);
    build_graph_compact(
        fragments,
        CompactEdges {
            node_to_idx,
            idx_to_node,
            edges: compact_edges,
        },
    )
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

    #[test]
    fn dedup_compact_edges_max_weight_first_category() {
        let mut edges = vec![
            CompactEdge {
                src: 0,
                dst: 1,
                weight: 0.5,
                category: EdgeCategory::Semantic,
            },
            CompactEdge {
                src: 0,
                dst: 1,
                weight: 0.8,
                category: EdgeCategory::Similarity,
            },
            CompactEdge {
                src: 2,
                dst: 1,
                weight: 0.3,
                category: EdgeCategory::Sibling,
            },
        ];
        dedup_compact_edges(&mut edges);
        assert_eq!(edges.len(), 2);
        assert!((edges[0].weight - 0.8).abs() < 1e-9);
        assert_eq!(edges[0].category, EdgeCategory::Semantic);
        assert_eq!(edges[1].category, EdgeCategory::Sibling);
    }
}
