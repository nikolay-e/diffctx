use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::selection::boltzmann;
use crate::select::{SelectionReason, SelectionResult};
use crate::types::{Fragment, FragmentId};

fn boltzmann_weight(rel_score: f64, token_count: u32, beta: f64) -> f64 {
    rel_score * (-beta * token_count as f64).exp()
}

fn nonoverlapping(selected: &[Fragment], frag: &Fragment) -> bool {
    selected.iter().all(|s| {
        if s.path() != frag.path() {
            return true;
        }
        frag.end_line() < s.start_line() || frag.start_line() > s.end_line()
    })
}

pub fn boltzmann_select(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    budget_tokens: u32,
    beta: f64,
) -> SelectionResult {
    if fragments.is_empty() {
        return SelectionResult {
            selected: Vec::new(),
            reason: SelectionReason::NoCandidates,
            used_tokens: 0,
            utility: 0.0,
            greedy_iters: 0,
        };
    }

    let mut core: Vec<Fragment> = fragments
        .iter()
        .filter(|f| core_ids.contains(&f.id))
        .cloned()
        .collect();
    core.sort_by_key(|f| f.token_count);

    let mut selected: Vec<Fragment> = Vec::new();
    let mut used: u32 = 0;
    for f in core {
        if used + f.token_count > budget_tokens {
            continue;
        }
        if !nonoverlapping(&selected, &f) {
            continue;
        }
        used += f.token_count;
        selected.push(f);
    }

    let mut ranked: Vec<(f64, Fragment)> = fragments
        .iter()
        .filter(|f| !core_ids.contains(&f.id) && f.token_count > 0)
        .map(|f| {
            let r = rel.get(&f.id).copied().unwrap_or(0.0);
            (boltzmann_weight(r, f.token_count, beta), f.clone())
        })
        .filter(|(w, _)| *w > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut total_utility = 0.0;
    let mut reason = SelectionReason::TopK;
    for (w, f) in ranked {
        if used + f.token_count > budget_tokens {
            reason = SelectionReason::BudgetExhausted;
            continue;
        }
        if !nonoverlapping(&selected, &f) {
            continue;
        }
        used += f.token_count;
        total_utility += w;
        selected.push(f);
    }

    let final_count = selected.len();
    SelectionResult {
        selected,
        reason,
        used_tokens: used,
        utility: total_utility,
        greedy_iters: final_count,
    }
}

pub fn calibrate_beta(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    budget_tokens: u32,
    epsilon: f64,
) -> f64 {
    let cfg = boltzmann();
    let (mut lo, mut hi) = (cfg.beta_lo, cfg.beta_hi);
    let target = budget_tokens as f64;
    let tol = (target * epsilon).max(1.0);

    // `boltzmann_select` budget-caps `used_tokens` at `target`, so the cost
    // function is monotone non-increasing in β and saturates at `target` for
    // small β. The well-defined calibration target is therefore "the largest
    // β whose selection still saturates the budget" — symmetric `|cost−B|<ε`
    // collapses to one-sided `cost ≥ B−tol` under this saturation. Both
    // bisection branches stay live: `lo=mid` advances when cost still
    // saturates (try larger β), `hi=mid` retreats when cost has dropped
    // (try smaller β). Loop invariant: `cost(lo) ≥ target−tol`.

    let cost_lo = boltzmann_select(fragments, core_ids, rel, budget_tokens, lo).used_tokens as f64;
    if cost_lo < target - tol {
        return lo;
    }
    let cost_hi = boltzmann_select(fragments, core_ids, rel, budget_tokens, hi).used_tokens as f64;
    if cost_hi >= target - tol {
        return hi;
    }

    for _ in 0..cfg.bisect_iters {
        let mid = (lo * hi).sqrt();
        let cost =
            boltzmann_select(fragments, core_ids, rel, budget_tokens, mid).used_tokens as f64;
        if cost >= target - tol {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod paper_claim_tests {
    //! Empirical validation of Boltzmann-related paper claims.
    //!
    //!  - Claim 8 (paper §4.5): selected cost is monotonically non-increasing in β
    //!    (β₁ < β₂ ⇒ E[c(C*(β₂))] ≤ E[c(C*(β₁))]).
    //!    Without monotonicity the bisection in `calibrate_beta` does not converge.

    use super::*;
    use crate::types::{FragmentId, FragmentKind};
    use std::sync::Arc;

    fn frag(name: &str, tokens: u32) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(format!("syn/{name}.rs")), 1, 10),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: tokens,
            symbol_name: Some(name.to_lowercase()),
        }
    }

    /// Tilde-utility under Boltzmann reweighting (paper §3, before greedy):
    /// $\tilde{U}(C) = \sum_{f \in C} w(f) \cdot e^{-\beta |f|}$.
    /// This must be modular — pure sum decomposition with no cross-term.
    fn tilde_utility(set: &[usize], fragments: &[Fragment], rels: &[f64], beta: f64) -> f64 {
        set.iter()
            .map(|&i| rels[i] * (-beta * fragments[i].token_count as f64).exp())
            .sum()
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn random_subset(n: usize, fraction: f64, rng: &mut u64) -> Vec<usize> {
        (0..n)
            .filter(|_| (xorshift(rng) % 1000) as f64 / 1000.0 < fraction)
            .collect()
    }

    #[test]
    fn claim_3a_boltzmann_reweighted_utility_is_modular() {
        let fragments: Vec<Fragment> = (0..10)
            .map(|i| frag(&format!("f_{i}"), 80 + i * 40))
            .collect();
        let rels: Vec<f64> = (0..10).map(|i| 0.2 + 0.07 * i as f64).collect();

        let mut rng = 0xBADCAFE_u64;
        for &beta in &[1e-4_f64, 1e-2, 0.1, 1.0, 10.0] {
            for _ in 0..1000 {
                let a = random_subset(10, 0.5, &mut rng);
                let b = random_subset(10, 0.5, &mut rng);
                let union: Vec<usize> = {
                    let s: FxHashSet<usize> = a.iter().chain(b.iter()).copied().collect();
                    let mut v: Vec<usize> = s.into_iter().collect();
                    v.sort();
                    v
                };
                let intersection: Vec<usize> = {
                    let sa: FxHashSet<usize> = a.iter().copied().collect();
                    let mut v: Vec<usize> = b.iter().copied().filter(|i| sa.contains(i)).collect();
                    v.sort();
                    v
                };
                let lhs = tilde_utility(&union, &fragments, &rels, beta);
                let rhs = tilde_utility(&a, &fragments, &rels, beta)
                    + tilde_utility(&b, &fragments, &rels, beta)
                    - tilde_utility(&intersection, &fragments, &rels, beta);
                assert!(
                    (lhs - rhs).abs() < 1e-9,
                    "Boltzmann tilde-utility not modular at β={beta}: \
                     lhs={lhs}, rhs={rhs}, |Δ|={}",
                    (lhs - rhs).abs()
                );
            }
        }
    }

    #[test]
    fn claim_8_boltzmann_cost_is_monotone_in_beta() {
        let fragments: Vec<Fragment> = (0..12)
            .map(|i| frag(&format!("f_{i}"), 50 + i * 60))
            .collect();
        let mut rels: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for (i, f) in fragments.iter().enumerate() {
            rels.insert(f.id.clone(), 0.2 + 0.05 * i as f64);
        }
        let core_ids = FxHashSet::default();
        let budget: u32 = 4096;

        let betas = [1e-6, 1e-4, 1e-2, 1.0, 100.0];
        let costs: Vec<u32> = betas
            .iter()
            .map(|&b| boltzmann_select(&fragments, &core_ids, &rels, budget, b).used_tokens)
            .collect();

        for i in 0..costs.len() - 1 {
            assert!(
                costs[i] >= costs[i + 1],
                "Boltzmann cost not monotone in β: at β={}→{} cost {}→{} (sequence: {costs:?})",
                betas[i],
                betas[i + 1],
                costs[i],
                costs[i + 1]
            );
        }

        assert!(
            costs[0] > costs[costs.len() - 1],
            "β sweep produced no cost variation; sweep range may be too narrow (costs: {costs:?})"
        );
    }
}
