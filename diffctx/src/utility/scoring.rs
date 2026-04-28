use std::path::PathBuf;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::UTILITY;
use crate::config::needs::NEEDS;
use crate::types::Fragment;
use crate::utility::needs::{InformationNeed, match_strength_typed};

pub struct UtilityState {
    pub max_rel: FxHashMap<(String, String), f64>,
    pub priorities: FxHashMap<(String, String), f64>,
    pub structural_sum: f64,
    pub eta: f64,
    pub structural_bonus_weight: f64,
    pub r_cap: f64,
    pub changed_dirs: FxHashSet<PathBuf>,
    pub proximity_decay: f64,
    pub file_importance: FxHashMap<Arc<str>, f64>,
}

impl Default for UtilityState {
    fn default() -> Self {
        Self {
            max_rel: FxHashMap::default(),
            priorities: FxHashMap::default(),
            structural_sum: 0.0,
            eta: UTILITY.eta,
            structural_bonus_weight: UTILITY.structural_bonus_weight,
            r_cap: 1.0,
            changed_dirs: FxHashSet::default(),
            proximity_decay: UTILITY.proximity_decay,
            file_importance: FxHashMap::default(),
        }
    }
}

impl UtilityState {
    pub fn copy(&self) -> Self {
        Self {
            max_rel: self.max_rel.clone(),
            priorities: self.priorities.clone(),
            structural_sum: self.structural_sum,
            eta: self.eta,
            structural_bonus_weight: self.structural_bonus_weight,
            r_cap: self.r_cap,
            changed_dirs: self.changed_dirs.clone(),
            proximity_decay: self.proximity_decay,
            file_importance: self.file_importance.clone(),
        }
    }
}

fn phi(x: f64) -> f64 {
    if x > 0.0 { x.sqrt() } else { 0.0 }
}

fn augmented_score(m: f64, rel_score: f64, state: &UtilityState) -> f64 {
    let r_norm = if state.r_cap > 0.0 {
        (rel_score / state.r_cap).min(1.0)
    } else {
        0.0
    };
    m + state.eta * r_norm
}

fn needs_from_identifiers(frag: &Fragment) -> Vec<InformationNeed> {
    frag.identifiers
        .iter()
        .map(|c| InformationNeed {
            need_type: "definition".to_string(),
            symbol: c.clone(),
            scope: None,
            priority: NEEDS.identifier_default_priority,
        })
        .collect()
}

struct GainResult {
    gain: f64,
    has_match: bool,
    need_updates: Vec<((String, String), f64, f64)>,
    diversity_bonus: f64,
    structural_bonus: f64,
}

fn diversity_bonus(
    needs: &[InformationNeed],
    rel_score: f64,
    gain: f64,
    state: &UtilityState,
) -> f64 {
    if needs.is_empty() || rel_score < NEEDS.min_rel_for_bonus {
        return 0.0;
    }
    if gain <= 0.0 {
        return 0.0;
    }
    let total_covered: f64 = needs
        .iter()
        .map(|n| {
            state
                .max_rel
                .get(&(n.need_type.clone(), n.symbol.clone()))
                .copied()
                .unwrap_or(0.0)
                .min(1.0)
        })
        .sum();
    let unsatisfied = (1.0 - total_covered / needs.len().max(1) as f64).max(0.0);
    rel_score * NEEDS.relatedness_bonus * unsatisfied
}

fn compute_gain_core(
    frag: &Fragment,
    rel_score: f64,
    needs: &[InformationNeed],
    state: &UtilityState,
    use_state_priorities: bool,
) -> GainResult {
    let effective: Vec<InformationNeed>;
    let needs_slice = if !needs.is_empty() {
        needs
    } else {
        effective = needs_from_identifiers(frag);
        &effective
    };

    let mut result = GainResult {
        gain: 0.0,
        has_match: false,
        need_updates: Vec::new(),
        diversity_bonus: 0.0,
        structural_bonus: 0.0,
    };

    if needs_slice.is_empty() {
        return result;
    }

    for need in needs_slice {
        let m = match_strength_typed(frag, need);
        if m <= 0.0 {
            continue;
        }
        let mut m_eff = m;
        if need.need_type == "impact" && !state.file_importance.is_empty() {
            let path_arc: Arc<str> = Arc::from(frag.path());
            m_eff *= state.file_importance.get(&path_arc).copied().unwrap_or(1.0);
        }
        result.has_match = true;
        let a_fz = augmented_score(m_eff, rel_score, state);
        let nkey = (need.need_type.clone(), need.symbol.clone());
        let old_max = state.max_rel.get(&nkey).copied().unwrap_or(0.0);
        let new_max = old_max.max(a_fz);
        let priority = if use_state_priorities {
            state
                .priorities
                .get(&nkey)
                .copied()
                .unwrap_or(need.priority)
        } else {
            need.priority
        };
        result.gain += priority * (phi(new_max) - phi(old_max));
        result.need_updates.push((nkey, new_max, need.priority));
    }

    result.diversity_bonus = diversity_bonus(needs, rel_score, result.gain, state);

    if result.has_match {
        let r_norm = if state.r_cap > 0.0 {
            (rel_score / state.r_cap).min(1.0)
        } else {
            0.0
        };
        result.structural_bonus = state.structural_bonus_weight * r_norm;
    }

    result
}

pub fn marginal_gain(
    frag: &Fragment,
    rel_score: f64,
    needs: &[InformationNeed],
    state: &UtilityState,
) -> f64 {
    let result = compute_gain_core(frag, rel_score, needs, state, false);
    result.gain + result.diversity_bonus + result.structural_bonus
}

pub fn apply_fragment(
    frag: &Fragment,
    rel_score: f64,
    needs: &[InformationNeed],
    state: &mut UtilityState,
) {
    let result = compute_gain_core(frag, rel_score, needs, state, true);
    for (nkey, new_max, priority) in result.need_updates {
        state.max_rel.insert(nkey.clone(), new_max);
        let current = state.priorities.get(&nkey).copied().unwrap_or(0.0);
        state.priorities.insert(nkey, current.max(priority));
    }
    state.structural_sum += result.diversity_bonus + result.structural_bonus;
}

fn dir_distance(d1: &std::path::Path, d2: &std::path::Path) -> usize {
    let p1: Vec<_> = d1.components().collect();
    let p2: Vec<_> = d2.components().collect();
    let mut common = 0;
    for (a, b) in p1.iter().zip(p2.iter()) {
        if a == b {
            common += 1;
        } else {
            break;
        }
    }
    (p1.len() - common) + (p2.len() - common)
}

fn proximity_factor(frag_path: &str, changed_dirs: &FxHashSet<PathBuf>, alpha: f64) -> f64 {
    if changed_dirs.is_empty() {
        return 1.0;
    }
    let frag_dir = std::path::Path::new(frag_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let min_dist = changed_dirs
        .iter()
        .map(|d| dir_distance(frag_dir, d))
        .min()
        .unwrap_or(0);
    if min_dist == 0 {
        return 1.0;
    }
    1.0 / (1.0 + alpha * min_dist as f64)
}

pub fn compute_density(
    frag: &Fragment,
    rel_score: f64,
    needs: &[InformationNeed],
    state: &UtilityState,
) -> f64 {
    if frag.token_count == 0 {
        return 0.0;
    }
    let gain = marginal_gain(frag, rel_score, needs, state);
    let pf = proximity_factor(frag.path(), &state.changed_dirs, state.proximity_decay);
    gain * pf / frag.token_count as f64
}

pub fn utility_value(state: &UtilityState) -> f64 {
    let u1: f64 = state
        .max_rel
        .iter()
        .map(|(sym, v)| {
            let p = state.priorities.get(sym).copied().unwrap_or(1.0);
            p * phi(*v)
        })
        .sum();
    u1 + state.structural_sum
}

#[cfg(test)]
mod paper_claim_tests {
    //! Empirical validation of theoretical claims (paper §3, §4.6).
    //!
    //! Each test maps to a numbered claim:
    //!  - Claim 1 (Theorem 1): U(C) = Σ_z φ(max_{f ∈ C} a_{f,z}) is submodular.
    //!  - Claim 4: cost(C) = Σ_f |f| is modular.

    use super::*;
    use crate::types::{FragmentId, FragmentKind};

    fn frag(symbol: &str, mentions: &[&str], tokens: u32) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(format!("syn/{symbol}.rs")), 1, 10),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: mentions.iter().map(|s| s.to_lowercase()).collect(),
            token_count: tokens,
            symbol_name: Some(symbol.to_lowercase()),
        }
    }

    fn need(symbol: &str, priority: f64) -> InformationNeed {
        InformationNeed {
            need_type: "definition".to_string(),
            symbol: symbol.to_lowercase(),
            scope: None,
            priority,
        }
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn random_subset_indices(n: usize, fraction: f64, rng: &mut u64) -> Vec<usize> {
        (0..n)
            .filter(|_| (xorshift(rng) % 1000) as f64 / 1000.0 < fraction)
            .collect()
    }

    fn build_state_from(
        fragments: &[Fragment],
        indices: &[usize],
        rels: &[f64],
        needs: &[InformationNeed],
    ) -> UtilityState {
        let mut state = UtilityState::default();
        for &i in indices {
            apply_fragment(&fragments[i], rels[i], needs, &mut state);
        }
        state
    }

    #[test]
    fn claim_4_cost_is_modular() {
        let fragments = vec![
            frag("alpha", &["beta"], 100),
            frag("beta", &["gamma"], 250),
            frag("gamma", &["alpha"], 75),
            frag("delta", &["alpha", "beta"], 500),
        ];

        let cost_of =
            |idx: &[usize]| -> u32 { idx.iter().map(|&i| fragments[i].token_count).sum() };

        let mut rng = 0xCAFEBABE_u64;
        for _ in 0..1000 {
            let a: Vec<usize> = random_subset_indices(fragments.len(), 0.5, &mut rng);
            let b: Vec<usize> = random_subset_indices(fragments.len(), 0.5, &mut rng);
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
            let lhs = cost_of(&union);
            let rhs = cost_of(&a) + cost_of(&b) - cost_of(&intersection);
            assert_eq!(
                lhs, rhs,
                "cost not modular: cost(A∪B)={lhs} ≠ cost(A)+cost(B)-cost(A∩B)={rhs}"
            );
        }
    }

    #[test]
    fn claim_1a_submodularity_holds_on_random_instances() {
        let symbols = [
            "foo", "bar", "baz", "qux", "alpha", "beta", "gamma", "delta",
        ];
        let fragments: Vec<Fragment> = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mentions: Vec<&str> = symbols.iter().take(i).copied().collect();
                frag(s, &mentions, 100 + (i as u32) * 50)
            })
            .collect();
        let needs: Vec<InformationNeed> = symbols.iter().map(|s| need(s, 1.0)).collect();
        let rels: Vec<f64> = (0..fragments.len()).map(|i| 0.3 + 0.1 * i as f64).collect();

        let mut rng = 0xDEADBEEF_u64;
        let mut violations = 0;
        let trials = 500;

        for _ in 0..trials {
            let s_idx: Vec<usize> = random_subset_indices(fragments.len(), 0.3, &mut rng);
            let mut t_idx: Vec<usize> = s_idx.clone();
            for j in 0..fragments.len() {
                if !t_idx.contains(&j) && (xorshift(&mut rng) % 100) < 40 {
                    t_idx.push(j);
                }
            }
            let candidates: Vec<usize> = (0..fragments.len())
                .filter(|i| !t_idx.contains(i))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let x = candidates[(xorshift(&mut rng) as usize) % candidates.len()];

            let state_s = build_state_from(&fragments, &s_idx, &rels, &needs);
            let state_t = build_state_from(&fragments, &t_idx, &rels, &needs);

            let marg_s = marginal_gain(&fragments[x], rels[x], &needs, &state_s);
            let marg_t = marginal_gain(&fragments[x], rels[x], &needs, &state_t);

            if marg_s + 1e-9 < marg_t {
                violations += 1;
                eprintln!("Submodularity violation: marg_S={marg_s:.6}, marg_T={marg_t:.6}, x={x}");
            }
        }

        assert_eq!(
            violations, 0,
            "Submodularity (Theorem 1) violated in {violations}/{trials} trials"
        );
    }

    #[test]
    fn claim_1b_saturation_zero_marginal_for_duplicate_definition() {
        let f1 = frag("foo", &[], 100);
        let f2 = frag("foo", &[], 100);
        let needs = vec![need("foo", 1.0)];

        let mut state = UtilityState::default();
        let m1 = marginal_gain(&f1, 1.0, &needs, &state);
        apply_fragment(&f1, 1.0, &needs, &mut state);
        let m2 = marginal_gain(&f2, 1.0, &needs, &state);

        assert!(
            m1 > 0.0,
            "first fragment must have positive marginal, got {m1}"
        );
        let bonus_only = state.structural_bonus_weight;
        assert!(
            m2 <= bonus_only + 1e-9,
            "duplicate definition must have ≈zero core gain marginal (only structural_bonus={bonus_only}); got {m2}"
        );
    }

    #[test]
    fn claim_2a_selected_fragments_never_overlap_within_a_path() {
        let mut fragments: Vec<Fragment> = Vec::new();
        for path_idx in 0..3 {
            for chunk_idx in 0..4 {
                let start = (chunk_idx * 20 + 1) as u32;
                let end = (chunk_idx * 20 + 15) as u32;
                let f = Fragment {
                    id: FragmentId::new(Arc::from(format!("p{path_idx}.rs")), start, end),
                    kind: FragmentKind::Function,
                    content: Arc::from(""),
                    identifiers: ["alpha", "beta"].iter().map(|s| s.to_string()).collect(),
                    token_count: 80 + (path_idx * 30 + chunk_idx * 10) as u32,
                    symbol_name: Some(format!("p{path_idx}_chunk_{chunk_idx}")),
                };
                fragments.push(f);
            }
            for chunk_idx in 0..3 {
                let start = (chunk_idx * 20 + 5) as u32;
                let end = (chunk_idx * 20 + 25) as u32;
                let f = Fragment {
                    id: FragmentId::new(Arc::from(format!("p{path_idx}.rs")), start, end),
                    kind: FragmentKind::Function,
                    content: Arc::from(""),
                    identifiers: ["alpha", "beta"].iter().map(|s| s.to_string()).collect(),
                    token_count: 90,
                    symbol_name: Some(format!("p{path_idx}_overlap_{chunk_idx}")),
                };
                fragments.push(f);
            }
        }
        let needs = vec![need("alpha", 1.0), need("beta", 1.0)];
        let mut rels: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for (i, f) in fragments.iter().enumerate() {
            rels.insert(f.id.clone(), 0.3 + 0.02 * i as f64);
        }
        let core_ids = FxHashSet::default();
        let budget = 2048;

        let result = crate::select::lazy_greedy_select(
            fragments.clone(),
            &core_ids,
            &rels,
            &needs,
            budget,
            0.08,
            None,
        );

        for (i, fi) in result.selected.iter().enumerate() {
            for fj in result.selected.iter().skip(i + 1) {
                if fi.id.path != fj.id.path {
                    continue;
                }
                let overlap =
                    fi.id.start_line <= fj.id.end_line && fj.id.start_line <= fi.id.end_line;
                assert!(
                    !overlap,
                    "Matroid (interval) constraint violated: \
                     {}:{}-{} overlaps with {}:{}-{}",
                    fi.id.path,
                    fi.id.start_line,
                    fi.id.end_line,
                    fj.id.path,
                    fj.id.start_line,
                    fj.id.end_line
                );
            }
        }
    }

    #[test]
    fn claim_5_greedy_meets_khuller_bound_against_brute_force_optimal() {
        let fragments: Vec<Fragment> = [
            ("foo", &[][..], 100u32),
            ("bar", &["foo"][..], 80),
            ("baz", &["bar"][..], 250),
            ("qux", &["foo", "bar"][..], 60),
            ("alpha", &["baz"][..], 400),
            ("beta", &["qux"][..], 90),
        ]
        .iter()
        .map(|(name, mentions, tokens)| frag(name, mentions, *tokens))
        .collect();

        let needs: Vec<InformationNeed> = ["foo", "bar", "baz", "qux", "alpha", "beta"]
            .iter()
            .map(|s| need(s, 1.0))
            .collect();

        let mut rels: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for (i, f) in fragments.iter().enumerate() {
            rels.insert(f.id.clone(), 0.4 + 0.1 * i as f64);
        }
        let core_ids: FxHashSet<FragmentId> = FxHashSet::default();
        let budget: u32 = 400;

        let n = fragments.len();
        let mut optimal: f64 = 0.0;
        for mask in 0u32..(1 << n) {
            let cost: u32 = (0..n)
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| fragments[i].token_count)
                .sum();
            if cost > budget {
                continue;
            }
            let mut state = UtilityState::default();
            for i in 0..n {
                if mask & (1 << i) != 0 {
                    apply_fragment(&fragments[i], rels[&fragments[i].id], &needs, &mut state);
                }
            }
            optimal = optimal.max(utility_value(&state));
        }

        let result = crate::select::lazy_greedy_select(
            fragments.clone(),
            &core_ids,
            &rels,
            &needs,
            budget,
            0.08,
            None,
        );
        let ratio = result.utility / optimal;
        let bound = 0.5 * (1.0 - 1.0_f64.exp().recip());
        assert!(
            ratio >= bound - 1e-6,
            "Khuller bound violated: greedy/optimal = {ratio:.4} < {bound:.4} (greedy={}, optimal={})",
            result.utility,
            optimal
        );
        assert!(
            ratio >= 0.5,
            "Realistic-data expectation violated: ratio={ratio:.4} should be ≥ 0.5 on this instance"
        );
    }

    #[test]
    fn claim_9_importance_prior_preserves_submodularity_for_impact_needs() {
        let mut fragments = vec![
            frag("hub", &["service"], 200),
            frag("client_a", &["hub"], 150),
            frag("client_b", &["hub"], 150),
            frag("client_c", &["hub"], 150),
        ];
        for f in &mut fragments {
            f.identifiers.insert("service".to_string());
        }
        let needs = vec![InformationNeed {
            need_type: "impact".to_string(),
            symbol: "service".to_string(),
            scope: None,
            priority: 1.0,
        }];
        let rels = vec![0.9, 0.7, 0.5, 0.3];

        let mut file_importance = FxHashMap::default();
        file_importance.insert(Arc::from("syn/hub.rs"), 0.5);
        file_importance.insert(Arc::from("syn/client_a.rs"), 1.0);
        file_importance.insert(Arc::from("syn/client_b.rs"), 0.8);
        file_importance.insert(Arc::from("syn/client_c.rs"), 0.6);

        let mut rng = 0x12345678_u64;
        for _ in 0..200 {
            let s_idx: Vec<usize> = random_subset_indices(fragments.len(), 0.3, &mut rng);
            let mut t_idx: Vec<usize> = s_idx.clone();
            for j in 0..fragments.len() {
                if !t_idx.contains(&j) && (xorshift(&mut rng) % 100) < 50 {
                    t_idx.push(j);
                }
            }
            let candidates: Vec<usize> = (0..fragments.len())
                .filter(|i| !t_idx.contains(i))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let x = candidates[(xorshift(&mut rng) as usize) % candidates.len()];

            let mut state_s = UtilityState {
                file_importance: file_importance.clone(),
                ..UtilityState::default()
            };
            for &i in &s_idx {
                apply_fragment(&fragments[i], rels[i], &needs, &mut state_s);
            }
            let mut state_t = UtilityState {
                file_importance: file_importance.clone(),
                ..UtilityState::default()
            };
            for &i in &t_idx {
                apply_fragment(&fragments[i], rels[i], &needs, &mut state_t);
            }

            let marg_s = marginal_gain(&fragments[x], rels[x], &needs, &state_s);
            let marg_t = marginal_gain(&fragments[x], rels[x], &needs, &state_t);
            assert!(
                marg_s + 1e-9 >= marg_t,
                "Importance prior breaks submodularity: marg_S={marg_s}, marg_T={marg_t}"
            );
        }
    }
}
