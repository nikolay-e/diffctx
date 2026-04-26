use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use std::collections::HashMap;

use crate::config::limits::LIMITS;
use crate::edges;
use crate::filtering;
use crate::graph::{self, Graph};
use crate::ppr::personalized_pagerank;
use crate::types::{DiffHunk, Fragment, FragmentId, extract_identifier_list};

const EGO_IDENT_OVERLAP_EPSILON: f64 = 0.1;
const EGO_IDENT_OVERLAP_CAP: usize = 10;
const BM25_K1: f64 = 2.5;
const BM25_B: f64 = 0.75;

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

        let filtered = filtering::filter_unrelated_fragments(all_fragments, core_ids, &g);
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

        let filtered = filtering::filter_unrelated_fragments(all_fragments, core_ids, &g);
        let filtered = filtering::filter_positive_relevance(filtered, core_ids, &rel_scores);
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
        }
    }
}

pub struct BM25Scoring;

impl ScoringStrategy for BM25Scoring {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        _hunks: &[DiffHunk],
        _repo_root: Option<&Path>,
        _seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        _discovered_paths: Option<&FxHashSet<Arc<str>>>,
    ) -> ScoringResult {
        let query_tokens: Vec<String> = all_fragments
            .iter()
            .filter(|f| core_ids.contains(&f.id))
            .flat_map(|f| extract_identifier_list(&f.content, 3))
            .collect();
        let query_set: FxHashSet<String> = query_tokens.into_iter().collect();

        let docs: Vec<(FragmentId, Vec<String>)> = all_fragments
            .iter()
            .filter(|f| !core_ids.contains(&f.id))
            .map(|f| (f.id.clone(), extract_identifier_list(&f.content, 3)))
            .collect();

        let n_docs = docs.len().max(1);
        let avgdl = docs.iter().map(|(_, d)| d.len()).sum::<usize>() as f64 / n_docs as f64;

        let mut df: HashMap<String, usize> = HashMap::new();
        for (_, doc) in &docs {
            let unique: FxHashSet<&str> = doc.iter().map(|s| s.as_str()).collect();
            for term in unique {
                *df.entry(term.to_string()).or_insert(0) += 1;
            }
        }

        let idf: FxHashMap<String, f64> = query_set
            .iter()
            .map(|t| {
                let d = df.get(t).copied().unwrap_or(0) as f64;
                let val = ((n_docs as f64 - d + 0.5) / (d + 0.5)).ln_1p();
                (t.clone(), val)
            })
            .collect();

        let mut rel_scores: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for frag in all_fragments {
            if core_ids.contains(&frag.id) {
                rel_scores.insert(frag.id.clone(), 1.0);
            }
        }
        for (fid, doc) in &docs {
            let dl = doc.len() as f64;
            let mut tf: HashMap<&str, u32> = HashMap::new();
            for t in doc {
                *tf.entry(t.as_str()).or_insert(0) += 1;
            }
            let mut score = 0.0;
            for t in &query_set {
                let freq = tf.get(t.as_str()).copied().unwrap_or(0) as f64;
                if freq == 0.0 {
                    continue;
                }
                let idf_val = idf.get(t).copied().unwrap_or(0.0);
                score += idf_val * (freq * BM25_K1)
                    / (freq + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl));
            }
            if score > 0.0 {
                rel_scores.insert(fid.clone(), score);
            }
        }

        let max_score = rel_scores.values().copied().fold(0.0f64, f64::max);
        if max_score > 0.0 {
            for v in rel_scores.values_mut() {
                *v /= max_score;
            }
        }

        let filtered: Vec<Fragment> = all_fragments
            .iter()
            .filter(|f| {
                core_ids.contains(&f.id) || rel_scores.get(&f.id).copied().unwrap_or(0.0) > 0.0
            })
            .cloned()
            .collect();
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        let g = Graph::new();
        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
        }
    }
}
