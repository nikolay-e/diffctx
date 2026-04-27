use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::selection::BOLTZMANN;
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

    SelectionResult {
        selected,
        reason,
        used_tokens: used,
        utility: total_utility,
    }
}

pub fn calibrate_beta(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    budget_tokens: u32,
    epsilon: f64,
) -> f64 {
    let (mut lo, mut hi) = (BOLTZMANN.beta_lo, BOLTZMANN.beta_hi);
    let target = budget_tokens as f64;
    let tol = (target * epsilon).max(1.0);

    for _ in 0..BOLTZMANN.bisect_iters {
        let mid = (lo * hi).sqrt();
        let result = boltzmann_select(fragments, core_ids, rel, budget_tokens, mid);
        let cost = result.used_tokens as f64;
        if cost > target + tol {
            lo = mid;
        } else if cost < target - tol {
            hi = mid;
        } else {
            return mid;
        }
    }
    (lo * hi).sqrt()
}
