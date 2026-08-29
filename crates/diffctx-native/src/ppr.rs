use std::collections::VecDeque;

use rayon;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::PPR;
use crate::graph::{CsrGraph, Graph};
use crate::types::FragmentId;

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
            .map(|s| sw.get(*s).copied().unwrap_or(PPR.default_seed_epsilon))
            .sum();
        if total <= 0.0 {
            return residual;
        }
        for s in &valid_seeds {
            let idx = csr.node_to_idx[*s] as usize;
            residual[idx] = sw.get(*s).copied().unwrap_or(PPR.default_seed_epsilon) / total;
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

/// Mirrors `init_seed_residuals`'s own validity check without building the
/// residual vector, so `personalized_pagerank` can tell "no seed mass ever
/// entered the push" apart from "the push ran and every score genuinely
/// converged to zero" -- both cases otherwise return the same empty
/// `scores` map with `forward_pushes == 0`, which is indistinguishable to
/// a caller that just sees an empty selection and assumes it means the
/// diff genuinely has no related context (a seed absent from the graph and
/// an explicit all-zero `seed_weights` map, reachable on a deletion-only
/// hunk, both hit this path).
fn has_valid_seed_mass(
    csr: &CsrGraph,
    seeds: &FxHashSet<FragmentId>,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> bool {
    let has_valid_seed = seeds.iter().any(|s| csr.node_to_idx.contains_key(s));
    if !has_valid_seed {
        return false;
    }
    match seed_weights {
        Some(sw) => {
            let total: f64 = seeds
                .iter()
                .filter(|s| csr.node_to_idx.contains_key(*s))
                .map(|s| sw.get(s).copied().unwrap_or(PPR.default_seed_epsilon))
                .sum();
            total > 0.0
        }
        None => true,
    }
}

/// Result of one PPR push pass. `truncated` flags whether the
/// `max_pushes` budget cut the iteration short — when true, the
/// returned estimate is biased toward seeds and the absolute scores
/// are not comparable to a converged run on the same graph. Surfaced
/// to Python via `LatencyBreakdown.ppr_truncated` so calibration /
/// final-eval rows can be filtered or flagged for the paper.
struct PprPushResult {
    estimate: Vec<f64>,
    pushes: usize,
    truncated: bool,
}

fn ppr_push_csr(
    csr: &CsrGraph,
    seeds: &FxHashSet<FragmentId>,
    alpha: f64,
    tol: f64,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> PprPushResult {
    let n = csr.n;
    if n == 0 {
        return PprPushResult {
            estimate: Vec::new(),
            pushes: 0,
            truncated: false,
        };
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

    let max_pushes = (n * PPR.push_scale_factor).min(PPR.max_pushes_cap);
    let mut pushes: usize = 0;
    let mut truncated = false;

    while let Some(u) = queue.pop_front() {
        if pushes >= max_pushes {
            truncated = true;
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

    PprPushResult {
        estimate,
        pushes,
        truncated,
    }
}

/// Public PPR result. `truncated` is logical-OR of forward + backward
/// push truncation flags. When true, downstream renormalization
/// (sum-to-1 across nodes) hides the fact that the iteration was cut
/// short by `max_pushes_cap`; the absolute relevance scores are
/// biased toward seeds and not directly comparable across instances.
/// We surface this to Python so calibration / final-eval rows can be
/// filtered in post-analysis (PolyBench / Multi-SWE-bench instances
/// with >20k fragments are the primary suspects).
pub struct PprResult {
    pub scores: FxHashMap<FragmentId, f64>,
    pub truncated: bool,
    pub forward_pushes: usize,
    pub backward_pushes: usize,
    /// False when no seed mass ever entered the push (no seed matched a
    /// graph node, or `seed_weights` summed to <= 0 across matched seeds).
    /// Lets the caller tell "nothing to seed from" apart from "seeded and
    /// converged to a genuinely empty `scores` map" -- both produce the
    /// same empty map and `forward_pushes == 0` otherwise.
    pub seeded: bool,
}

pub fn personalized_pagerank(
    graph: &mut Graph,
    seeds: &FxHashSet<FragmentId>,
    alpha: f64,
    tol: f64,
    forward_blend: f64,
    seed_weights: Option<&FxHashMap<FragmentId, f64>>,
) -> PprResult {
    if graph.node_count() == 0 || seeds.is_empty() {
        return PprResult {
            scores: FxHashMap::default(),
            truncated: false,
            forward_pushes: 0,
            backward_pushes: 0,
            seeded: false,
        };
    }

    let (fwd_csr, rev_csr) = graph.to_csr();
    let seeded = has_valid_seed_mass(fwd_csr, seeds, seed_weights);

    let (forward, backward) = rayon::join(
        || ppr_push_csr(fwd_csr, seeds, alpha, tol, seed_weights),
        || ppr_push_csr(rev_csr, seeds, alpha, tol, seed_weights),
    );

    let n = fwd_csr.n;
    let mut combined = vec![0.0f64; n];
    for i in 0..n {
        combined[i] =
            forward_blend * forward.estimate[i] + (1.0 - forward_blend) * backward.estimate[i];
    }

    let total: f64 = combined.iter().sum();
    if total > 0.0 {
        for v in &mut combined {
            *v /= total;
        }
    }

    let idx_to_node = &fwd_csr.idx_to_node;
    let mut scores: FxHashMap<FragmentId, f64> = FxHashMap::default();
    for i in 0..n {
        if combined[i] > 0.0 {
            scores.insert(idx_to_node[i].clone(), combined[i]);
        }
    }

    PprResult {
        scores,
        truncated: forward.truncated || backward.truncated,
        forward_pushes: forward.pushes,
        backward_pushes: backward.pushes,
        seeded,
    }
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
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-4, 0.4, None).scores;
        assert!(result.is_empty());
    }

    #[test]
    fn ppr_single_node() {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        g.add_node(a.clone());

        let mut seeds = FxHashSet::default();
        seeds.insert(a.clone());
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-4, 0.4, None).scores;
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
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, None).scores;

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
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, None).scores;

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

        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.4, Some(&sw)).scores;
        assert!(result[&a] > result[&b]);
    }

    fn build_star_graph() -> (Graph, FragmentId) {
        let mut g = Graph::new();
        let center = fid("center.rs", 1, 10);
        g.add_node(center.clone());
        for i in 0..5 {
            let leaf = fid(&format!("leaf_{i}.rs"), 1, 10);
            g.add_node(leaf.clone());
            g.add_edge(center.clone(), leaf.clone(), 1.0);
            g.add_edge(leaf.clone(), center.clone(), 1.0);
        }
        (g, center)
    }

    #[test]
    fn ppr_is_deterministic_across_calls() {
        let (mut g1, center) = build_star_graph();
        let (mut g2, _) = build_star_graph();
        let seeds: FxHashSet<FragmentId> = std::iter::once(center).collect();

        let r1 = personalized_pagerank(&mut g1, &seeds, 0.6, 1e-6, 0.4, None).scores;
        let r2 = personalized_pagerank(&mut g2, &seeds, 0.6, 1e-6, 0.4, None).scores;

        assert_eq!(r1.len(), r2.len());
        for (id, v1) in &r1 {
            let v2 = r2.get(id).copied().unwrap_or(f64::NAN);
            assert!((v1 - v2).abs() < 1e-12, "PPR drift at {id}: {v1} vs {v2}");
        }
    }

    #[test]
    fn ppr_converges_under_tighter_tolerance() {
        let (mut g_loose, center) = build_star_graph();
        let (mut g_tight, _) = build_star_graph();
        let seeds: FxHashSet<FragmentId> = std::iter::once(center).collect();

        let loose = personalized_pagerank(&mut g_loose, &seeds, 0.6, 1e-2, 0.4, None).scores;
        let tight = personalized_pagerank(&mut g_tight, &seeds, 0.6, 1e-6, 0.4, None).scores;

        let max_diff = loose
            .iter()
            .map(|(id, v)| (v - tight.get(id).copied().unwrap_or(0.0)).abs())
            .fold(0.0f64, f64::max);
        assert!(
            max_diff < 1e-2,
            "PPR did not converge: max diff between tol=1e-2 and tol=1e-6 is {max_diff}"
        );
    }

    #[test]
    fn ppr_symmetric_star_assigns_equal_mass_to_leaves() {
        let (mut g, center) = build_star_graph();
        let seeds: FxHashSet<FragmentId> = std::iter::once(center.clone()).collect();
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-8, 0.5, None).scores;

        let leaf_scores: Vec<f64> = (0..5)
            .map(|i| result[&fid(&format!("leaf_{i}.rs"), 1, 10)])
            .collect();
        let max_leaf = leaf_scores.iter().cloned().fold(0.0f64, f64::max);
        let min_leaf = leaf_scores.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(
            (max_leaf - min_leaf) < 1e-6,
            "Symmetric star should give equal leaf mass; got spread {} (leaves: {leaf_scores:?})",
            max_leaf - min_leaf
        );
        assert!(result[&center] > max_leaf, "Center must dominate leaves");
    }

    /// Claim 7 (paper §4.4): hub suppression $w'_{uv} = w_{uv} / \ln(1 + \text{in\_deg}(v))$
    /// reduces PPR mass concentration on hub nodes without removing them.
    ///
    /// Two graphs are compared:
    ///   - Naive: a leaf-to-hub graph built directly via `Graph::add_edge`, no suppression.
    ///   - Suppressed: same topology built via `build_graph` with non-exempt category,
    ///     which triggers `apply_hub_suppression` for in-degree above the median.
    ///
    /// Expected: hub mass is materially reduced; non-hub mass is largely preserved.
    #[test]
    fn claim_7_hub_suppression_reduces_hub_mass_without_removal() {
        use crate::graph::{EdgeCategory, build_graph};
        use crate::types::{Fragment, FragmentKind};

        let n_leaves = 20usize;
        let hub = fid("hub.rs", 1, 10);
        let leaves: Vec<FragmentId> = (0..n_leaves)
            .map(|i| fid(&format!("leaf_{i}.rs"), 1, 10))
            .collect();

        let mut naive = Graph::new();
        naive.add_node(hub.clone());
        for leaf in &leaves {
            naive.add_node(leaf.clone());
            naive.add_edge(leaf.clone(), hub.clone(), 1.0);
        }
        for i in 0..n_leaves - 1 {
            naive.add_edge(leaves[i].clone(), leaves[i + 1].clone(), 1.0);
        }

        let fragments: Vec<Fragment> = std::iter::once(hub.clone())
            .chain(leaves.iter().cloned())
            .map(|id| Fragment {
                id,
                kind: FragmentKind::Function,
                content: Arc::from(""),
                identifiers: FxHashSet::default(),
                token_count: 100,
                symbol_name: None,
            })
            .collect();
        let mut edges: FxHashMap<(FragmentId, FragmentId), f64> = FxHashMap::default();
        let mut categories: FxHashMap<(FragmentId, FragmentId), EdgeCategory> =
            FxHashMap::default();
        for leaf in &leaves {
            edges.insert((leaf.clone(), hub.clone()), 1.0);
            categories.insert((leaf.clone(), hub.clone()), EdgeCategory::Generic);
        }
        for i in 0..n_leaves - 1 {
            edges.insert((leaves[i].clone(), leaves[i + 1].clone()), 1.0);
            categories.insert(
                (leaves[i].clone(), leaves[i + 1].clone()),
                EdgeCategory::Generic,
            );
        }
        let mut suppressed = build_graph(&fragments, edges, categories);

        let seeds: FxHashSet<FragmentId> = leaves.iter().take(3).cloned().collect();
        let alpha = 0.6;
        let tol = 1e-8;
        let blend = 1.0;

        let r_naive = personalized_pagerank(&mut naive, &seeds, alpha, tol, blend, None).scores;
        let r_suppressed =
            personalized_pagerank(&mut suppressed, &seeds, alpha, tol, blend, None).scores;

        let hub_naive = r_naive.get(&hub).copied().unwrap_or(0.0);
        let hub_suppressed = r_suppressed.get(&hub).copied().unwrap_or(0.0);

        assert!(
            hub_suppressed < hub_naive,
            "Hub suppression did not reduce hub mass: naive={hub_naive}, suppressed={hub_suppressed}"
        );
        let reduction_ratio = hub_naive / hub_suppressed.max(1e-12);
        assert!(
            reduction_ratio >= 1.5,
            "Hub suppression effect too small: only {reduction_ratio:.2}× reduction (want ≥1.5×)"
        );

        assert!(
            hub_suppressed > 0.0,
            "Hub mass should be reduced, not removed; got {hub_suppressed}"
        );

        let mut leaves_present_after = 0;
        for leaf in &leaves {
            if r_suppressed.get(leaf).copied().unwrap_or(0.0) > 0.0 {
                leaves_present_after += 1;
            }
        }
        assert!(
            leaves_present_after >= leaves.len() / 2,
            "Suppression should preserve most leaves in the result, only {leaves_present_after}/{} survived",
            leaves.len()
        );
    }

    /// Claim 10 (paper §4.4 hypothesis): PPR with $\alpha \in [0.5, 0.65]$ ranks nodes
    /// similarly to ego-graph BFS scoring (hop-decay 1/(1+d)). Spearman rank correlation
    /// of the two score vectors should be high on a connected graph.
    #[test]
    fn claim_10_ppr_and_ego_rankings_correlate_on_synthetic_graph() {
        let mut g = Graph::new();
        let nodes: Vec<FragmentId> = (0..30).map(|i| fid(&format!("n_{i}.rs"), 1, 10)).collect();
        for n in &nodes {
            g.add_node(n.clone());
        }
        let mut rng = 0xC0FFEE_u64;
        let xorshift = crate::test_rng::xorshift;
        for i in 0..nodes.len() {
            for _ in 0..3 {
                let j = (xorshift(&mut rng) as usize) % nodes.len();
                if i != j {
                    g.add_edge(nodes[i].clone(), nodes[j].clone(), 1.0);
                }
            }
        }
        for i in 0..nodes.len() {
            let next = (i + 1) % nodes.len();
            g.add_edge(nodes[i].clone(), nodes[next].clone(), 1.0);
        }

        let seeds: FxHashSet<FragmentId> = nodes.iter().take(2).cloned().collect();
        let ppr_scores = personalized_pagerank(&mut g, &seeds, 0.6, 1e-8, 0.5, None).scores;
        let ego_scores = g.ego_graph(&seeds, 3);

        let common: Vec<FragmentId> = nodes
            .iter()
            .filter(|n| ppr_scores.contains_key(n) && ego_scores.contains_key(n))
            .cloned()
            .collect();
        assert!(
            common.len() >= 10,
            "Need ≥10 common ranked nodes, got {}",
            common.len()
        );

        let ppr_v: Vec<f64> = common.iter().map(|n| ppr_scores[n]).collect();
        let ego_v: Vec<f64> = common.iter().map(|n| ego_scores[n]).collect();

        let rho = spearman_correlation(&ppr_v, &ego_v);
        assert!(
            rho > 0.3,
            "PPR/EGO Spearman correlation too low: ρ={rho:.3} (paper hypothesizes high correlation, want > 0.3)"
        );
    }

    fn rank(values: &[f64]) -> Vec<f64> {
        let n = values.len();
        let mut indexed: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut ranks = vec![0.0_f64; n];
        let mut i = 0;
        while i < n {
            let mut j = i;
            while j + 1 < n && indexed[j + 1].1 == indexed[i].1 {
                j += 1;
            }
            let avg_rank = ((i + j) as f64) / 2.0 + 1.0;
            for k in i..=j {
                ranks[indexed[k].0] = avg_rank;
            }
            i = j + 1;
        }
        ranks
    }

    fn spearman_correlation(x: &[f64], y: &[f64]) -> f64 {
        assert_eq!(x.len(), y.len());
        let rx = rank(x);
        let ry = rank(y);
        let n = x.len() as f64;
        let mean_x: f64 = rx.iter().sum::<f64>() / n;
        let mean_y: f64 = ry.iter().sum::<f64>() / n;
        let mut cov = 0.0;
        let mut var_x = 0.0;
        let mut var_y = 0.0;
        for i in 0..rx.len() {
            let dx = rx[i] - mean_x;
            let dy = ry[i] - mean_y;
            cov += dx * dy;
            var_x += dx * dx;
            var_y += dy * dy;
        }
        cov / (var_x.sqrt() * var_y.sqrt()).max(1e-12)
    }

    /// Claim 6B (paper §4.4): personalized PageRank converges to the closed-form
    /// stationary distribution π = (1-α)(I - αM)^(-1) p, normalized to sum to 1.
    ///
    /// For a symmetric star with center c and N leaves, all bidirectional weight 1:
    ///     π_center = 1 / (1 + α)
    ///     π_leaf   = α / (N · (1 + α))
    /// Derivation:
    ///     π_c = (1-α) + α · Σ_leaf π_leaf,   π_leaf = α · (1/N) · π_c
    ///     ⇒ π_c · (1 + α) = 1 after normalization (Σ π = 1).
    #[test]
    fn ppr_matches_closed_form_on_symmetric_star() {
        for &(alpha, n_leaves) in &[(0.6, 5usize), (0.5, 4usize), (0.85, 7usize)] {
            let mut g = Graph::new();
            let center = fid("center.rs", 1, 10);
            g.add_node(center.clone());
            for i in 0..n_leaves {
                let leaf = fid(&format!("leaf_{i}.rs"), 1, 10);
                g.add_node(leaf.clone());
                g.add_edge(center.clone(), leaf.clone(), 1.0);
                g.add_edge(leaf.clone(), center.clone(), 1.0);
            }
            let seeds: FxHashSet<FragmentId> = std::iter::once(center.clone()).collect();
            let result = personalized_pagerank(&mut g, &seeds, alpha, 1e-10, 0.5, None).scores;

            let expected_center = 1.0 / (1.0 + alpha);
            let expected_leaf = alpha / (n_leaves as f64 * (1.0 + alpha));

            let actual_center = result[&center];
            let center_err = (actual_center - expected_center).abs();
            assert!(
                center_err < 1e-3,
                "α={alpha}, N={n_leaves}: center mass drift |actual - closed-form| = {center_err}; \
                 expected={expected_center}, got={actual_center}"
            );

            for i in 0..n_leaves {
                let leaf_id = fid(&format!("leaf_{i}.rs"), 1, 10);
                let actual_leaf = result[&leaf_id];
                let leaf_err = (actual_leaf - expected_leaf).abs();
                assert!(
                    leaf_err < 1e-3,
                    "α={alpha}, N={n_leaves}, leaf_{i}: drift |actual - closed-form| = {leaf_err}; \
                     expected={expected_leaf}, got={actual_leaf}"
                );
            }
        }
    }

    fn build_three_cycle() -> Graph {
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a, b.clone(), 1.0);
        g.add_edge(b, c.clone(), 1.0);
        g.add_edge(c, fid("a.rs", 1, 10), 1.0);
        g
    }

    /// `max_pushes = n * PPR.push_scale_factor` (3*100=300 for this
    /// fixture). A high alpha (slow restart) plus a tight tolerance
    /// forces far more pushes than that budget, so the push loop must
    /// stop early and report `truncated`. Deleting `truncated = true;`
    /// in `ppr_push_csr` would make this pass silently -- the pinned
    /// `forward_pushes` count is what actually catches that deletion,
    /// since the flag alone can't distinguish "stopped early" from "ran
    /// exactly to convergence at push 300".
    #[test]
    fn ppr_truncates_under_tight_tolerance_and_high_alpha() {
        let mut g = build_three_cycle();
        let a = fid("a.rs", 1, 10);
        let seeds: FxHashSet<FragmentId> = std::iter::once(a).collect();

        let result = personalized_pagerank(&mut g, &seeds, 0.99, 1e-12, 0.5, None);

        assert!(
            result.truncated,
            "alpha=0.99, tol=1e-12 on a 3-cycle must exhaust the push budget"
        );
        assert_eq!(
            result.forward_pushes, 300,
            "pinned push count for this fixture; a regression in max_pushes or the push loop \
             would move this number"
        );
    }

    #[test]
    fn ppr_does_not_truncate_under_loose_tolerance() {
        let mut g = build_three_cycle();
        let a = fid("a.rs", 1, 10);
        let seeds: FxHashSet<FragmentId> = std::iter::once(a).collect();

        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-6, 0.5, None);

        assert!(
            !result.truncated,
            "alpha=0.6, tol=1e-6 on a 3-cycle must converge well within the push budget"
        );
    }

    #[test]
    fn ppr_chain_with_dangling_terminal_node_sums_to_one() {
        // a -> b -> c, c has no outgoing edges (dangling). None of the
        // existing fixtures (full cycle, symmetric star) has a node with
        // zero out-weight; `ppr_push_csr`'s `total_w <= 0.0` branch is
        // only exercised here.
        let mut g = Graph::new();
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let c = fid("c.rs", 1, 10);
        g.add_node(a.clone());
        g.add_node(b.clone());
        g.add_node(c.clone());
        g.add_edge(a.clone(), b.clone(), 1.0);
        g.add_edge(b.clone(), c.clone(), 1.0);

        let seeds: FxHashSet<FragmentId> = std::iter::once(a).collect();
        let result = personalized_pagerank(&mut g, &seeds, 0.6, 1e-8, 0.5, None);

        assert!(result.seeded);
        let total: f64 = result.scores.values().sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "mass must still sum to 1.0 despite the dangling terminal node; got {total}"
        );
    }

    #[test]
    fn ppr_seed_absent_from_graph_is_distinguishable_from_zero_seed_weights() {
        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let ghost = fid("ghost.rs", 1, 10);

        // Baseline: a real, present, positively-weighted seed produces a
        // non-empty, seeded result.
        let mut g_present = Graph::new();
        g_present.add_node(a.clone());
        g_present.add_node(b.clone());
        g_present.add_edge(a.clone(), b.clone(), 1.0);
        let seeds_present: FxHashSet<FragmentId> = std::iter::once(a.clone()).collect();
        let present = personalized_pagerank(&mut g_present, &seeds_present, 0.6, 1e-6, 0.5, None);
        assert!(present.seeded);
        assert!(!present.scores.is_empty());

        // A seed id that matches no node in the graph.
        let mut g_absent = Graph::new();
        g_absent.add_node(a.clone());
        g_absent.add_node(b.clone());
        g_absent.add_edge(a.clone(), b.clone(), 1.0);
        let seeds_absent: FxHashSet<FragmentId> = std::iter::once(ghost).collect();
        let absent = personalized_pagerank(&mut g_absent, &seeds_absent, 0.6, 1e-6, 0.5, None);
        assert!(
            !absent.seeded,
            "a seed id absent from the graph must not be reported as seeded"
        );
        assert!(absent.scores.is_empty());
        assert_eq!(absent.forward_pushes, 0);

        // A seed that IS in the graph, but `seed_weights` zeroes it out --
        // reachable when every changed line in a hunk maps to a deleted
        // fragment. Same empty `scores` / zero pushes as the absent-seed
        // case above, but `seeded` must still tell them apart.
        let mut g_zeroed = Graph::new();
        g_zeroed.add_node(a.clone());
        g_zeroed.add_node(b.clone());
        g_zeroed.add_edge(a.clone(), b.clone(), 1.0);
        let seeds_zeroed: FxHashSet<FragmentId> = std::iter::once(a.clone()).collect();
        let mut zero_weights: FxHashMap<FragmentId, f64> = FxHashMap::default();
        zero_weights.insert(a, 0.0);
        let zeroed = personalized_pagerank(
            &mut g_zeroed,
            &seeds_zeroed,
            0.6,
            1e-6,
            0.5,
            Some(&zero_weights),
        );
        assert!(
            !zeroed.seeded,
            "an all-zero seed_weights map must not be reported as seeded"
        );
        assert!(zeroed.scores.is_empty());
        assert_eq!(zeroed.forward_pushes, 0);

        assert_eq!(
            (absent.scores.len(), absent.forward_pushes, absent.seeded),
            (zeroed.scores.len(), zeroed.forward_pushes, zeroed.seeded),
            "both degenerate cases produce identical scores/pushes -- `seeded` is currently the \
             only signal telling them apart from each other and from real convergence-to-zero"
        );
    }

    #[test]
    fn ppr_excludes_self_loop_and_infinite_weight_and_stays_finite() {
        use crate::graph::{EdgeCategory, build_graph};
        use crate::types::{Fragment, FragmentKind};

        let a = fid("a.rs", 1, 10);
        let b = fid("b.rs", 1, 10);
        let plain = |id: FragmentId| Fragment {
            id,
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: 10,
            symbol_name: None,
        };
        let frags = vec![plain(a.clone()), plain(b.clone())];

        let mut edges: FxHashMap<(FragmentId, FragmentId), f64> = FxHashMap::default();
        let mut cats: FxHashMap<(FragmentId, FragmentId), EdgeCategory> = FxHashMap::default();
        edges.insert((a.clone(), b.clone()), 1.0);
        cats.insert((a.clone(), b.clone()), EdgeCategory::Semantic);
        edges.insert((a.clone(), a.clone()), 5.0); // self-loop
        cats.insert((a.clone(), a.clone()), EdgeCategory::Semantic);
        edges.insert((b.clone(), b.clone()), f64::INFINITY); // self-loop AND non-finite
        cats.insert((b.clone(), b.clone()), EdgeCategory::Semantic);

        let mut graph = build_graph(&frags, edges, cats);
        let seeds: FxHashSet<FragmentId> = std::iter::once(a).collect();
        let result = personalized_pagerank(&mut graph, &seeds, 0.6, 1e-6, 0.5, None);

        assert!(result.seeded);
        assert!(
            !result.scores.is_empty(),
            "a self-loop / infinite-weight edge must not poison out_weight_sum into NaN and \
             empty out the whole score map"
        );
        for (id, score) in &result.scores {
            assert!(
                score.is_finite(),
                "score for {id} must be finite, got {score}"
            );
        }
    }
}
