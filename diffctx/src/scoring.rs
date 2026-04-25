use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::LIMITS;
use crate::edges;
use crate::filtering;
use crate::graph::{self, Graph};
use crate::ppr::personalized_pagerank;
use crate::types::{DiffHunk, Fragment, FragmentId};

const EGO_IDENT_OVERLAP_EPSILON: f64 = 0.1;
const EGO_IDENT_OVERLAP_CAP: usize = 10;

pub struct ScoringResult {
    pub rel_scores: FxHashMap<FragmentId, f64>,
    pub filtered_fragments: Vec<Fragment>,
    pub graph: Graph,
}

pub trait ScoringStrategy: Send + Sync {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        discovered_paths: Option<&FxHashSet<Arc<str>>>,
    ) -> ScoringResult;
}

pub struct PPRScoring {
    pub alpha: f64,
    pub low_relevance_filter: bool,
}

impl PPRScoring {
    pub fn new(alpha: f64, low_relevance_filter: bool) -> Self {
        Self {
            alpha,
            low_relevance_filter,
        }
    }
}

impl ScoringStrategy for PPRScoring {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        _discovered_paths: Option<&FxHashSet<Arc<str>>>,
    ) -> ScoringResult {
        let skip_expensive = all_fragments.len() > LIMITS.skip_expensive_threshold;
        let (edges, categories) =
            edges::collect_all_edges(all_fragments, repo_root, skip_expensive);
        let mut g = graph::build_graph(all_fragments, edges, categories);
        let mut rel_scores =
            personalized_pagerank(&mut g, core_ids, self.alpha, 1e-4, 0.4, seed_weights);
        filtering::apply_hunk_proximity_bonus(&mut rel_scores, core_ids, all_fragments, hunks);

        let filtered = filtering::filter_unrelated_fragments(all_fragments.to_vec(), core_ids, &g);
        let filtered = if self.low_relevance_filter {
            filtering::filter_low_relevance(filtered, core_ids, &rel_scores)
        } else {
            filtering::filter_positive_relevance(filtered, core_ids, &rel_scores)
        };
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
        }
    }
}

pub struct EgoGraphScoring {
    pub max_depth: usize,
}

impl EgoGraphScoring {
    pub fn new(max_depth: usize) -> Self {
        Self { max_depth }
    }
}

impl ScoringStrategy for EgoGraphScoring {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        _hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        _seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        _discovered_paths: Option<&FxHashSet<Arc<str>>>,
    ) -> ScoringResult {
        let skip_expensive = all_fragments.len() > LIMITS.skip_expensive_threshold;
        let (edges, categories) =
            edges::collect_all_edges(all_fragments, repo_root, skip_expensive);
        let g = graph::build_graph(all_fragments, edges, categories);
        let mut rel_scores = g.ego_graph(core_ids, self.max_depth);

        let diff_idents: FxHashSet<String> = all_fragments
            .iter()
            .filter(|f| core_ids.contains(&f.id))
            .flat_map(|f| f.identifiers.iter().cloned())
            .collect();

        if !diff_idents.is_empty() {
            for frag in all_fragments {
                if core_ids.contains(&frag.id) || !rel_scores.contains_key(&frag.id) {
                    continue;
                }
                let overlap = frag.identifiers.intersection(&diff_idents).count();
                if overlap > 0 {
                    let bonus = EGO_IDENT_OVERLAP_EPSILON
                        * overlap.min(EGO_IDENT_OVERLAP_CAP) as f64
                        / EGO_IDENT_OVERLAP_CAP as f64;
                    *rel_scores.get_mut(&frag.id).unwrap() += bonus;
                }
            }
        }

        let filtered = filtering::filter_unrelated_fragments(all_fragments.to_vec(), core_ids, &g);
        let filtered = filtering::filter_positive_relevance(filtered, core_ids, &rel_scores);
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
        }
    }
}
