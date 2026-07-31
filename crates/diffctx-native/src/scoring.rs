use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::bm25::BM25;
use crate::config::limits::{LIMITS, PPR};
use crate::config::scoring::{EGO, rrf};
use crate::config::tokenization::TOKENIZATION;
use crate::edges;
use crate::filtering;
use crate::graph::{self, Graph};
use crate::mode::{PipelineConfig, ScoringKind};
use crate::ppr::personalized_pagerank;
use crate::types::{DiffHunk, Fragment, FragmentId, extract_identifier_list};

pub struct ScoringResult {
    pub rel_scores: FxHashMap<FragmentId, f64>,
    pub filtered_fragments: Vec<Fragment>,
    pub graph: Graph,
    /// Wall time spent constructing the typed dependency graph (edge
    /// builders + dedup + hub suppression + per-source cap). Reported
    /// separately so `scoring_ms` stays pure rank computation. Zero for
    /// BM25 (no graph built).
    pub graph_build_ms: f64,
    /// PPR push-iteration was cut by `max_pushes_cap` before convergence.
    /// Always false for non-PPR strategies (EGO/BM25).
    pub ppr_truncated: bool,
    pub ppr_forward_pushes: usize,
    pub ppr_backward_pushes: usize,
}

pub fn create_scoring_strategy(config: &PipelineConfig) -> Box<dyn ScoringStrategy> {
    match config.scoring {
        ScoringKind::Ego => Box::new(EgoGraphScoring::new(config.ego_depth)),
        ScoringKind::Ppr => Box::new(PPRScoring::new(config.ppr_alpha)),
        ScoringKind::Bm25 => Box::new(BM25Scoring),
        ScoringKind::Rrf => Box::new(RrfFusionScoring::new(config.ego_depth)),
    }
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
}

impl PPRScoring {
    pub fn new(alpha: f64) -> Self {
        Self { alpha }
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
        let t_graph = Instant::now();
        let capped = edges::collect_capped_edges(all_fragments, repo_root, skip_expensive);
        let mut g = graph::build_graph_capped(all_fragments, capped);
        let graph_build_ms = t_graph.elapsed().as_secs_f64() * 1000.0;
        let ppr = personalized_pagerank(
            &mut g,
            core_ids,
            self.alpha,
            PPR.convergence_tolerance,
            PPR.forward_blend,
            seed_weights,
        );
        let mut rel_scores = ppr.scores;
        if ppr.truncated {
            tracing::warn!(
                "PPR push-cap hit on {} nodes (fwd_pushes={}, bwd_pushes={}); rel_scores biased",
                g.node_count(),
                ppr.forward_pushes,
                ppr.backward_pushes,
            );
        }
        filtering::apply_hunk_proximity_bonus(&mut rel_scores, core_ids, all_fragments, hunks);

        let filtered = filtering::filter_unrelated_fragments(all_fragments, core_ids, &g);
        let filtered = filtering::filter_positive_relevance(filtered, core_ids, &rel_scores);
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
            graph_build_ms,
            ppr_truncated: ppr.truncated,
            ppr_forward_pushes: ppr.forward_pushes,
            ppr_backward_pushes: ppr.backward_pushes,
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
        let t_graph = Instant::now();
        let capped = edges::collect_capped_edges(all_fragments, repo_root, skip_expensive);
        let g = graph::build_graph_capped(all_fragments, capped);
        let graph_build_ms = t_graph.elapsed().as_secs_f64() * 1000.0;
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
                    let bonus = EGO.identifier_overlap_epsilon
                        * overlap.min(EGO.identifier_overlap_cap) as f64
                        / EGO.identifier_overlap_cap as f64;
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
            graph_build_ms,
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
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
            .flat_map(|f| {
                extract_identifier_list(&f.content, TOKENIZATION.query_min_identifier_length)
            })
            .collect();
        let query_set: FxHashSet<String> = query_tokens.into_iter().collect();

        let docs: Vec<(FragmentId, Vec<String>)> = all_fragments
            .iter()
            .filter(|f| !core_ids.contains(&f.id))
            .map(|f| {
                (
                    f.id.clone(),
                    extract_identifier_list(&f.content, TOKENIZATION.query_min_identifier_length),
                )
            })
            .collect();

        let n_docs = docs.len().max(1);
        let avgdl = docs.iter().map(|(_, d)| d.len()).sum::<usize>() as f64 / n_docs as f64;

        let mut df: FxHashMap<String, usize> = FxHashMap::default();
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
                let val =
                    ((n_docs as f64 - d + BM25.idf_smoothing) / (d + BM25.idf_smoothing)).ln_1p();
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
            let mut tf: FxHashMap<&str, u32> = FxHashMap::default();
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
                score += idf_val * (freq * BM25.k1)
                    / (freq + BM25.k1 * (1.0 - BM25.b + BM25.b * dl / avgdl));
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
            graph_build_ms: 0.0,
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
        }
    }
}

/// Reciprocal-rank fusion of the structural (EGO) and lexical (BM25) signals.
///
/// The two are complementary rather than redundant: on genuine
/// retrieval the lexical component alone outranks the deployed
/// graph+lexical mixture, and their score-free union raises the reachable
/// recall well above either — a miscalibrated-mixture signature. RRF
/// fuses on ranks only, so neither component's score scale can dominate
/// the other, which is exactly the failure the weighted mixture had.
pub struct RrfFusionScoring {
    pub ego_depth: usize,
    pub k: f64,
}

impl RrfFusionScoring {
    pub fn new(ego_depth: usize) -> Self {
        Self {
            ego_depth,
            k: rrf().k,
        }
    }
}

fn rank_positions(
    rel: &FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
) -> FxHashMap<FragmentId, usize> {
    let mut ranked: Vec<(&FragmentId, f64)> = rel
        .iter()
        .filter(|(fid, score)| **score > 0.0 && !core_ids.contains(*fid))
        .map(|(fid, score)| (fid, *score))
        .collect();
    // Ties broken by id so the rank list — and therefore every fused
    // score — is independent of hash-map iteration order.
    ranked.sort_by(|(ida, sa), (idb, sb)| sb.total_cmp(sa).then_with(|| ida.cmp(idb)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(i, (fid, _))| (fid.clone(), i + 1))
        .collect()
}

fn fuse_reciprocal_ranks(
    components: &[&FxHashMap<FragmentId, f64>],
    core_ids: &FxHashSet<FragmentId>,
    k: f64,
) -> FxHashMap<FragmentId, f64> {
    let mut fused: FxHashMap<FragmentId, f64> = FxHashMap::default();
    for rel in components {
        for (fid, rank) in rank_positions(rel, core_ids) {
            *fused.entry(fid).or_insert(0.0) += 1.0 / (k + rank as f64);
        }
    }

    let max_fused = fused.values().copied().fold(0.0f64, f64::max);
    if max_fused > 0.0 {
        for v in fused.values_mut() {
            *v /= max_fused;
        }
    }
    // Cores anchor the top of the scale, matching every other strategy —
    // downstream `r_cap` normalisation and the absolute relevance gates
    // read these values, so the fused range has to stay [0, 1].
    for fid in core_ids {
        fused.insert(fid.clone(), 1.0);
    }
    fused
}

impl ScoringStrategy for RrfFusionScoring {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        discovered_paths: Option<&FxHashSet<Arc<str>>>,
    ) -> ScoringResult {
        let ego = EgoGraphScoring::new(self.ego_depth).score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
        );
        let lexical = BM25Scoring.score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
        );

        let rel_scores =
            fuse_reciprocal_ranks(&[&ego.rel_scores, &lexical.rel_scores], core_ids, self.k);

        let mut union_ids: FxHashSet<FragmentId> = ego
            .filtered_fragments
            .iter()
            .map(|f| f.id.clone())
            .collect();
        union_ids.extend(lexical.filtered_fragments.iter().map(|f| f.id.clone()));

        let union: Vec<Fragment> = all_fragments
            .iter()
            .filter(|f| union_ids.contains(&f.id))
            .cloned()
            .collect();

        // The union re-admits paths that EGO's structural guards dropped
        // (hub noise, generic-config-only code), because BM25 has no graph
        // to judge them by. Re-applying the guards keeps the fusion a
        // recall gain rather than a precision regression, and the per-file
        // cap has to be recomputed against the fused scores since each
        // component capped against its own.
        let filtered = filtering::filter_unrelated_fragments(&union, core_ids, &ego.graph);
        let filtered = filtering::filter_positive_relevance(filtered, core_ids, &rel_scores);
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            rel_scores,
            filtered_fragments: filtered,
            graph: ego.graph,
            graph_build_ms: ego.graph_build_ms,
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
        }
    }
}
