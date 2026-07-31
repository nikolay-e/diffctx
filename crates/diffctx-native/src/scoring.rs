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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    fn fid(path: &str, start: u32) -> FragmentId {
        FragmentId::new(Arc::from(path), start, start + 4)
    }

    fn scores(entries: &[(FragmentId, f64)]) -> FxHashMap<FragmentId, f64> {
        entries.iter().cloned().collect()
    }

    /// The property RRF exists for, and the reason `k` damps the top of each
    /// list: agreement between the two signals beats a single signal's first
    /// place. A weighted mixture cannot express this without calibrating the two
    /// score scales against each other — which is the failure RRF replaces.
    #[test]
    fn agreement_between_both_signals_outranks_a_single_signal_top_hit() {
        let agreed = fid("agreed.rs", 1);
        let ego_only = fid("ego_only.rs", 1);
        let cores: FxHashSet<FragmentId> = FxHashSet::default();

        // `ego_only` is rank 1 in ego and absent from bm25; `agreed` is only
        // rank 2 in each, but present in both.
        let ego = scores(&[(ego_only.clone(), 0.9), (agreed.clone(), 0.5)]);
        let lexical = scores(&[(agreed.clone(), 0.5)]);

        let fused = fuse_reciprocal_ranks(&[&ego, &lexical], &cores, 60.0);
        assert!(
            fused[&agreed] > fused[&ego_only],
            "agreement lost to a single-signal top hit: {:?} vs {:?}",
            fused[&agreed],
            fused[&ego_only]
        );
    }

    /// Only ranks may cross between the components. If a raw score leaked in,
    /// one signal's scale could dominate the other — the miscalibrated-mixture
    /// behaviour the mode was added to avoid.
    #[test]
    fn only_the_rank_order_of_a_component_matters_not_its_scale() {
        let a = fid("a.rs", 1);
        let b = fid("b.rs", 1);
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        let lexical = scores(&[(a.clone(), 0.4), (b.clone(), 0.1)]);

        let modest = scores(&[(a.clone(), 0.6), (b.clone(), 0.4)]);
        let enormous = scores(&[(a.clone(), 6_000.0), (b.clone(), 4_000.0)]);

        assert_eq!(
            fuse_reciprocal_ranks(&[&modest, &lexical], &cores, 60.0),
            fuse_reciprocal_ranks(&[&enormous, &lexical], &cores, 60.0),
            "rescaling one component changed the fused scores"
        );
    }

    /// Downstream `r_cap` normalisation and the absolute relevance gates read
    /// these values, so the fused range has to stay within [0, 1] with the cores
    /// at the top.
    #[test]
    fn fused_scores_are_normalised_and_cores_anchor_the_top() {
        let core = fid("changed.rs", 1);
        let ctx = fid("ctx.rs", 1);
        let cores: FxHashSet<FragmentId> = std::iter::once(core.clone()).collect();
        let ego = scores(&[(core.clone(), 1.0), (ctx.clone(), 0.3)]);
        let lexical = scores(&[(ctx.clone(), 0.2)]);

        let fused = fuse_reciprocal_ranks(&[&ego, &lexical], &cores, 60.0);
        assert_eq!(fused[&core], 1.0, "core is not anchored at the top");
        for (id, score) in &fused {
            assert!(
                (0.0..=1.0).contains(score),
                "{id:?} scored {score} outside [0, 1]"
            );
        }
    }

    /// Cores are ranked separately (they are always placed first), so letting
    /// them consume rank slots would push every context fragment down and change
    /// the fused scores for reasons unrelated to relevance.
    #[test]
    fn cores_do_not_occupy_rank_positions() {
        let core = fid("changed.rs", 1);
        let ctx = fid("ctx.rs", 1);
        let cores: FxHashSet<FragmentId> = std::iter::once(core.clone()).collect();

        let with_core = scores(&[(core.clone(), 1.0), (ctx.clone(), 0.3)]);
        let without_core = scores(&[(ctx.clone(), 0.3)]);

        assert_eq!(
            rank_positions(&with_core, &cores).get(&ctx),
            rank_positions(&without_core, &cores).get(&ctx),
            "a core shifted the rank of a context fragment"
        );
    }

    /// Hash-map iteration order must not reach the fused scores: equal
    /// component scores are ranked by fragment id.
    #[test]
    fn equal_component_scores_rank_deterministically() {
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        let ids: Vec<FragmentId> = (0..8).map(|i| fid(&format!("f{i}.rs"), 1)).collect();
        let tied: FxHashMap<FragmentId, f64> = ids.iter().map(|i| (i.clone(), 0.5)).collect();

        let baseline = rank_positions(&tied, &cores);
        for _ in 0..8 {
            let again: FxHashMap<FragmentId, f64> =
                ids.iter().rev().map(|i| (i.clone(), 0.5)).collect();
            assert_eq!(rank_positions(&again, &cores), baseline);
        }

        // Ascending id order is the tie-break, so ranks follow the sorted ids.
        let mut sorted = ids.clone();
        sorted.sort();
        for (expected_rank, id) in sorted.iter().enumerate() {
            assert_eq!(baseline[id], expected_rank + 1);
        }
    }

    /// A zero or negative component score is "not a candidate", not "ranked
    /// last": including it would hand it a reciprocal-rank contribution.
    #[test]
    fn non_positive_component_scores_are_not_ranked() {
        let kept = fid("kept.rs", 1);
        let zero = fid("zero.rs", 1);
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        let component = scores(&[(kept.clone(), 0.3), (zero.clone(), 0.0)]);

        let ranks = rank_positions(&component, &cores);
        assert!(ranks.contains_key(&kept));
        assert!(
            !ranks.contains_key(&zero),
            "a zero-scored fragment was ranked"
        );

        let fused = fuse_reciprocal_ranks(&[&component], &cores, 60.0);
        assert!(
            !fused.contains_key(&zero),
            "a zero-scored fragment was fused"
        );
    }

    /// `DIFFCTX_RRF_K` is a documented knob; a larger k flattens the reciprocal
    /// curve, which is what makes agreement outweigh a single top hit.
    #[test]
    fn a_larger_k_flattens_the_gap_between_adjacent_ranks() {
        let first = fid("first.rs", 1);
        let second = fid("second.rs", 1);
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        let component = scores(&[(first.clone(), 0.9), (second.clone(), 0.8)]);

        let sharp = fuse_reciprocal_ranks(&[&component], &cores, 1.0);
        let flat = fuse_reciprocal_ranks(&[&component], &cores, 60.0);

        // Both are max-normalised, so compare the runner-up's share of the top.
        assert!(
            flat[&second] > sharp[&second],
            "k did not flatten adjacent ranks: {} vs {}",
            flat[&second],
            sharp[&second]
        );
    }

    #[test]
    fn a_strategy_is_created_for_every_scoring_kind() {
        // A new mode that forgets its arm here would silently score as another.
        for mode in [
            crate::mode::ScoringMode::Ego,
            crate::mode::ScoringMode::Ppr,
            crate::mode::ScoringMode::Bm25,
            crate::mode::ScoringMode::Rrf,
        ] {
            let config = PipelineConfig::from_mode(mode);
            let strategy = create_scoring_strategy(&config);
            let empty: Vec<Fragment> = Vec::new();
            let result =
                strategy.score_and_filter(&empty, &FxHashSet::default(), &[], None, None, None);
            assert!(
                result.filtered_fragments.is_empty(),
                "{mode:?} invented fragments from an empty universe"
            );
        }
        // Keeps `FragmentKind` in scope for the helper above.
        let _ = FragmentKind::Function;
    }
}
