use std::collections::VecDeque;

use rayon;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::{CsrGraph, Graph};
use crate::types::FragmentId;

const DEFAULT_SEED_EPSILON: f64 = 0.1;

fn init_seed_residuals(
    csr: &CsrGraph,
    seeds: &FxHashSet<FragmentId>,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> Vec<f64> {
    let n = csr.n;
    let mut residual = vec![0.0f64; n];

    let valid_seeds: Vec<&FragmentId> = seeds
        .iter()
        .filter(|s| csr.node_to_idx.contains_key(*s))
        .collect();

    if valid_seeds.is_empty() {
        return residual;
    }

    if let Some(sw) = seed_weights {
        let total: f64 = valid_seeds
            .iter()
            .map(|s| sw.get(*s).copied().unwrap_or(DEFAULT_SEED_EPSILON))
            .sum();
        if total <= 0.0 {
            return residual;
        }
        for s in &valid_seeds {
            let idx = csr.node_to_idx[*s] as usize;
            residual[idx] = sw.get(*s).copied().unwrap_or(DEFAULT_SEED_EPSILON) / total;
        }
    } else {
        let weight = 1.0 / valid_seeds.len() as f64;
        for s in &valid_seeds {
            let idx = csr.node_to_idx[*s] as usize;
            residual[idx] = weight;
        }
    }

    residual
}

fn ppr_push_csr(
    csr: &CsrGraph,
    seeds: &FxHashSet<FragmentId>,
    alpha: f64,
    tol: f64,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> Vec<f64> {
    let n = csr.n;
    if n == 0 {
        return Vec::new();
    }

    let restart = 1.0 - alpha;
    let mut residual = init_seed_residuals(csr, seeds, seed_weights);
    let mut estimate = vec![0.0f64; n];
    let mut in_queue = vec![false; n];

    let mut queue: VecDeque<u32> = VecDeque::new();
    for i in 0..n {
        if residual[i] >= tol {
            queue.push_back(i as u32);
            in_queue[i] = true;
        }
    }

    let max_pushes = (n * 100).min(2_000_000);
    let mut pushes: usize = 0;

    while let Some(u) = queue.pop_front() {
        if pushes >= max_pushes {
            break;
        }
        let ui = u as usize;
        in_queue[ui] = false;

        let r_u = residual[ui];
        if r_u < tol {
            continue;
        }

        estimate[ui] += restart * r_u;
        residual[ui] = 0.0;

        let total_w = csr.out_weight_sum[ui];
        if total_w <= 0.0 {
            pushes += 1;
            continue;
        }

        let propagate = alpha * r_u;
        let start = csr.indptr[ui] as usize;
        let end = csr.indptr[ui + 1] as usize;

        for k in start..end {
            let v = csr.indices[k] as usize;
            let w = csr.weights[k];
            let delta = propagate * (w / total_w);
            residual[v] += delta;
            if !in_queue[v] && residual[v] >= tol {
                queue.push_back(v as u32);
                in_queue[v] = true;
            }
        }

        pushes += 1;
    }

    estimate
}

pub fn personalized_pagerank(
    graph: &mut Graph,
    seeds: &FxHashSet<FragmentId>,
    alpha: f64,
    tol: f64,
    lam: f64,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> FxHashMap<FragmentId, f64> {
    if graph.node_count() == 0 {
        return FxHashMap::default();
    }

    if seeds.is_empty() {
        return FxHashMap::default();
    }

    let (fwd_csr, rev_csr) = graph.to_csr();

    let (forward_est, backward_est) = rayon::join(
        || ppr_push_csr(fwd_csr, seeds, alpha, tol, seed_weights),
        || ppr_push_csr(rev_csr, seeds, alpha, tol, seed_weights),
    );

    let n = fwd_csr.n;
    let mut combined = vec![0.0f64; n];
    for i in 0..n {
        combined[i] = lam * forward_est[i] + (1.0 - lam) * backward_est[i];
    }

    let total: f64 = combined.iter().sum();
    if total > 0.0 {
        for v in &mut combined {
            *v /= total;
        }
    }

    let idx_to_node = &fwd_csr.idx_to_node;
    let mut result: FxHashMap<FragmentId, f64> = FxHashMap::default();
    for i in 0..n {
        if combined[i] > 0.0 {
            result.insert(idx_to_node[i].clone(), combined[i]);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fid(path: &str, start: u32, end: u32) -> FragmentId {
        FragmentId::new(Arc::from(path), start, end)
    }

    #[test]
    fn ppr_empty_graph() {
        let mut g = Graph::new();
        let seeds = FxHashSet::default();
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-4, 0.4, None);
        assert!(result.is_empty());
    }

    #[test]
    fn ppr_single_node() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        g.add_node(a.clone());

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-4, 0.4, None);
        assert!((result[&a] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ppr_chain_scores_decrease() {
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
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, None);

        assert!(result[&a] > result[&b]);
        assert!(result[&b] > result[&c]);
    }

    #[test]
    fn ppr_normalizes_to_one() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a.clone(), b.clone(), 1.0);
        g.add_edge(b.clone(), c.clone(), 1.0);
        g.add_edge(c.clone(), a.clone(), 0.5);

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, None);

        let total: f64 = result.values().sum();
        assert!((total - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ppr_with_seed_weights() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_edge(a.clone(), b.clone(), 1.0);

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        seeds.insert(b.clone());

        let mut sw = FxHashMap::default();
        sw.insert(a.clone(), 0.9);
        sw.insert(b.clone(), 0.1);

        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, Some(&sw));
        assert!(result[&a] > result[&b]);
    }
}
