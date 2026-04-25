use std::path::PathBuf;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::UTILITY;
use crate::types::Fragment;
use crate::utility::needs::{InformationNeed, match_strength_typed};

const MIN_REL_FOR_BONUS: f64 = 0.03;
const RELATEDNESS_BONUS: f64 = 0.25;

pub struct UtilityState {
    pub max_rel: FxHashMap<(String, String), f64>,
    pub priorities: FxHashMap<(String, String), f64>,
    pub structural_sum: f64,
    pub eta: f64,
    pub gamma: f64,
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
            gamma: UTILITY.gamma,
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
            gamma: self.gamma,
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
            priority: 0.5,
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
    if needs.is_empty() || rel_score < MIN_REL_FOR_BONUS {
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
    rel_score * RELATEDNESS_BONUS * unsatisfied
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
        result.structural_bonus = state.gamma * r_norm;
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
