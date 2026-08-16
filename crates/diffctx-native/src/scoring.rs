use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::bm25::BM25;
use crate::config::edge_weights::SEMANTIC_DISCOVERY;
use crate::config::limits::{LIMITS, PPR};
use crate::config::scoring::{EGO, pit, rrf};
use crate::config::tokenization::TOKENIZATION;
use crate::edges;
use crate::filtering;
use crate::graph::{self, Graph};
use crate::mode::{PipelineConfig, ScoringKind};
use crate::ppr::personalized_pagerank;
use crate::types::{DiffHunk, Fragment, FragmentId, extract_identifier_list};

/// Per-file naming admission (#65) is the default since the v5 cycle:
/// screening (12-cell grid), calibration + held-out validation, and the
/// confirmation sweep all passed the pre-registered criteria. Opt out with
/// DIFFCTX_FILE_ADMISSION=0.
pub(crate) fn file_admission_enabled() -> bool {
    std::env::var_os("DIFFCTX_FILE_ADMISSION").is_none_or(|v| v != "0")
}

pub struct ScoringResult {
    pub rel_scores: FxHashMap<FragmentId, f64>,
    pub filtered_fragments: Vec<Fragment>,
    /// Files openable by the greedy under per-file admission (#65): reachable
    /// from the core set via naming-class edges. None = admission off (flag
    /// unset or a strategy without a typed graph), every file admissible.
    pub admissible_files: Option<FxHashSet<std::sync::Arc<str>>>,
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

/// The guard chain every graph-backed strategy ends with: drop what the graph
/// says is unrelated, drop what scored zero, then cap per file.
///
/// EGO, PPR and RRF each spelled this out. BM25 spelled out its own copy of the
/// middle step instead of calling it — semantically the same today, and exactly
/// the shape that let the two test-file classifiers drift apart (#182).
fn finish_scoring(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel_scores: &FxHashMap<FragmentId, f64>,
    graph: &Graph,
) -> Vec<Fragment> {
    let filtered = filtering::filter_unrelated_fragments(fragments, core_ids, graph);
    let filtered = filtering::filter_positive_relevance(filtered, core_ids, rel_scores);
    let filtered = filtering::filter_core_slice_context(filtered, core_ids);
    filtering::cap_context_fragments(filtered, core_ids, rel_scores)
}

impl Default for ScoringResult {
    fn default() -> Self {
        Self {
            rel_scores: FxHashMap::default(),
            filtered_fragments: Vec::new(),
            admissible_files: None,
            graph: Graph::new(),
            graph_build_ms: 0.0,
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
        }
    }
}

pub fn create_scoring_strategy(config: &PipelineConfig) -> Box<dyn ScoringStrategy> {
    match config.scoring {
        ScoringKind::Ego => Box::new(EgoGraphScoring::new(config.ego_depth)),
        ScoringKind::Ppr => Box::new(PPRScoring::new(config.ppr_alpha)),
        ScoringKind::Bm25 => Box::new(BM25Scoring),
        ScoringKind::Rrf => Box::new(RrfFusionScoring::new(config.ego_depth)),
        ScoringKind::Pit => Box::new(PitFusionScoring::new(config.ego_depth)),
    }
}

pub trait ScoringStrategy: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        discovered_paths: Option<&FxHashSet<Arc<str>>>,
        deadline: crate::deadline::Deadline,
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
        deadline: crate::deadline::Deadline,
    ) -> ScoringResult {
        let skip_expensive = all_fragments.len() > LIMITS.skip_expensive_threshold;
        let t_graph = Instant::now();
        let capped =
            edges::collect_capped_edges(all_fragments, repo_root, skip_expensive, deadline);
        let admissible_files = file_admission_enabled().then(|| {
            edges::naming_reachable_files(&capped, core_ids, SEMANTIC_DISCOVERY.max_depth)
        });
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

        let filtered = finish_scoring(all_fragments, core_ids, &rel_scores, &g);

        ScoringResult {
            admissible_files,
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
        deadline: crate::deadline::Deadline,
    ) -> ScoringResult {
        let skip_expensive = all_fragments.len() > LIMITS.skip_expensive_threshold;
        let t_graph = Instant::now();
        let capped =
            edges::collect_capped_edges(all_fragments, repo_root, skip_expensive, deadline);
        let admissible_files = file_admission_enabled().then(|| {
            edges::naming_reachable_files(&capped, core_ids, SEMANTIC_DISCOVERY.max_depth)
        });
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

        let filtered = finish_scoring(all_fragments, core_ids, &rel_scores, &g);

        ScoringResult {
            admissible_files,
            rel_scores,
            filtered_fragments: filtered,
            graph: g,
            graph_build_ms,
            ..Default::default()
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
        _deadline: crate::deadline::Deadline,
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

        // Deliberately NOT `finish_scoring`: there is no graph here, so the
        // structural guard has nothing to judge with. The other two steps are
        // the shared ones rather than a local re-spelling of the same predicate.
        let filtered =
            filtering::filter_positive_relevance(all_fragments.to_vec(), core_ids, &rel_scores);
        let filtered = filtering::cap_context_fragments(filtered, core_ids, &rel_scores);

        ScoringResult {
            admissible_files: None,
            rel_scores,
            filtered_fragments: filtered,
            ..Default::default()
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

/// A component's ballot: the fragments it admitted, ordered by its own score.
///
/// `admitted` is the component's `filtered_fragments`, not its whole score map.
/// Rank fusion is defined over the result lists retrievers return, and each
/// component's guards are the only place an *absolute* judgement survives —
/// a rank cannot express "this scored near zero". Ranking the full score map
/// instead lets a component vote for fragments its own filters rejected, and
/// reciprocal rank then promotes that tail: BM25 scores anything sharing a
/// generic token, so garbage landed at a respectable rank and earned real
/// fused mass. Measured on the oracle corpus, that cost 97 cases against EGO
/// on precision (`forbidden_rate >= 90%` on 91 of them) while recall held.
fn rank_positions(
    rel: &FxHashMap<FragmentId, f64>,
    admitted: &FxHashSet<FragmentId>,
    core_ids: &FxHashSet<FragmentId>,
) -> FxHashMap<FragmentId, usize> {
    let mut ranked: Vec<(&FragmentId, f64)> = rel
        .iter()
        .filter(|(fid, score)| **score > 0.0 && !core_ids.contains(*fid) && admitted.contains(*fid))
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
    components: &[(&FxHashMap<FragmentId, f64>, &FxHashSet<FragmentId>)],
    core_ids: &FxHashSet<FragmentId>,
    k: f64,
) -> FxHashMap<FragmentId, f64> {
    let mut fused: FxHashMap<FragmentId, f64> = FxHashMap::default();
    for (rel, admitted) in components {
        for (fid, rank) in rank_positions(rel, admitted, core_ids) {
            *fused.entry(fid).or_insert(0.0) += 1.0 / (k + rank as f64);
        }
    }

    let max_fused = fused.values().copied().fold(0.0f64, f64::max);
    if max_fused > 0.0 {
        for v in fused.values_mut() {
            *v /= max_fused;
        }
    }
    // Cores sit at the top of the scale, matching every other strategy —
    // downstream `r_cap` normalisation and the absolute relevance gates read
    // these values, so the fused range has to stay [0, 1]. Note they do not
    // strictly dominate: max-normalisation already put the best non-core at
    // exactly 1.0, so it ties with the cores rather than sitting below them.
    // `r_cap` excludes cores when it computes its spread, so the tie is benign
    // there — but do not read this as a guarantee that cores rank first.
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
        deadline: crate::deadline::Deadline,
    ) -> ScoringResult {
        let ego = EgoGraphScoring::new(self.ego_depth).score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
            deadline,
        );
        let lexical = BM25Scoring.score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
            deadline,
        );

        let ego_admitted: FxHashSet<FragmentId> = ego
            .filtered_fragments
            .iter()
            .map(|f| f.id.clone())
            .collect();
        let lexical_admitted: FxHashSet<FragmentId> = lexical
            .filtered_fragments
            .iter()
            .map(|f| f.id.clone())
            .collect();

        let rel_scores = fuse_reciprocal_ranks(
            &[
                (&ego.rel_scores, &ego_admitted),
                (&lexical.rel_scores, &lexical_admitted),
            ],
            core_ids,
            self.k,
        );

        let union_ids: FxHashSet<FragmentId> =
            ego_admitted.union(&lexical_admitted).cloned().collect();

        let union: Vec<Fragment> = all_fragments
            .iter()
            .filter(|f| union_ids.contains(&f.id))
            .cloned()
            .collect();

        // The union re-admits paths that EGO's structural guards dropped
        // (hub noise, generic-config-only code), because BM25 has no graph
        // to judge them by. The guards are re-applied and the per-file cap
        // recomputed against the fused scores, since each component capped
        // against its own.
        //
        // Measured caveat, not a claim of soundness: re-applying them does NOT
        // make the union a net win. On the oracle corpus RRF loses 97 cases to
        // EGO and gains 18, all on precision, and restricting candidates to
        // EGO's admitted set recovers only 28 of the 82 (#125).
        //
        // A second, unmeasured degree of freedom lives here: each component
        // already applied `cap_context_fragments` (30/file) against its own
        // scores before voting, so a fragment the fused ranking would have kept
        // can have been capped away before it ever reached the ballot. The cap
        // is per file and the losses are cross-file, so this is unlikely to be
        // the 97 — but it has not been isolated.
        let filtered = finish_scoring(&union, core_ids, &rel_scores, &ego.graph);

        ScoringResult {
            // The fusion inherits the ego component's naming-reachability set:
            // admission is a graph property, and the fusion's structural half
            // IS that graph. Leaving this None silently ran fusion arms
            // without the gate.
            admissible_files: ego.admissible_files.clone(),
            rel_scores,
            filtered_fragments: filtered,
            graph: ego.graph,
            graph_build_ms: ego.graph_build_ms,
            ..Default::default()
        }
    }
}

/// Percentile fusion: the same two signals as RRF, blended on their empirical
/// distribution position rather than on rank alone.
///
/// RRF converts each component to a pure rank, which throws away the magnitude
/// that says "this scored near zero". Measured on the oracle corpus that costs
/// 97 cases against EGO and wins 18, all on precision: BM25 gives any
/// generic-token match a small positive score, and `1/(k + rank)` promotes that
/// noise to real fused mass (#125).
///
/// The probability-integral transform keeps the position. A fragment in the 5th
/// percentile of a component contributes 0.05 from it, not `1/(k + 12)`. Two
/// signals that disagree therefore cannot manufacture a strong candidate out of
/// two weak opinions, which is precisely what the rank form allowed.
///
/// `score = blend * PIT(ego) + (1 - blend) * PIT(bm25) + bonus * [both in top-k]`
///
/// The agreement term is what fusion is actually for — a fragment both signals
/// rank highly is more trustworthy than either alone — and it is additive and
/// small so it breaks ties rather than deciding the ranking.
pub struct PitFusionScoring {
    pub ego_depth: usize,
    pub blend: f64,
    pub agreement_bonus: f64,
    pub agreement_top_k: usize,
}

impl PitFusionScoring {
    pub fn new(ego_depth: usize) -> Self {
        let cfg = pit();
        Self {
            ego_depth,
            blend: cfg.blend,
            agreement_bonus: cfg.agreement_bonus,
            agreement_top_k: cfg.agreement_top_k,
        }
    }
}

/// Empirical-CDF position in `[0, 1]` for every admitted, positively-scored
/// fragment, plus the set that sits in the component's own top-k.
///
/// Ties share a percentile: two fragments a component cannot separate must not
/// be separated here either, or the blend would invent a preference the signal
/// never expressed.
///
/// The CDF is estimated over everything the component scored positively, while
/// only the fragments it *admitted* receive a value. Those are two different
/// questions and conflating them was a defect: a percentile read off a
/// component's admitted set is a position within that set, and the two
/// components' admitted sets differ by an order of magnitude on a real repo
/// (BM25 admits a handful, EGO hundreds). Blending a position-among-6 with a
/// position-among-300 as if they were the same quantity is not a fusion of the
/// two signals. The admission veto itself is kept — a fragment a component
/// rejected still contributes nothing from it (#125, `091c4db3`) — because the
/// component's own guards are the only place an absolute judgement survives.
fn percentiles(
    rel: &FxHashMap<FragmentId, f64>,
    admitted: &FxHashSet<FragmentId>,
    core_ids: &FxHashSet<FragmentId>,
    top_k: usize,
) -> (FxHashMap<FragmentId, f64>, FxHashSet<FragmentId>) {
    let mut ranked: Vec<(&FragmentId, f64)> = rel
        .iter()
        .filter(|(fid, score)| **score > 0.0 && !core_ids.contains(*fid))
        .map(|(fid, score)| (fid, *score))
        .collect();
    // Descending by score, id as the tie-break so the traversal is independent
    // of hash-map iteration order.
    ranked.sort_by(|(ida, sa), (idb, sb)| sb.total_cmp(sa).then_with(|| ida.cmp(idb)));

    let n = ranked.len();
    let mut out: FxHashMap<FragmentId, f64> = FxHashMap::default();
    let mut top: FxHashSet<FragmentId> = FxHashSet::default();
    if n == 0 {
        return (out, top);
    }

    // Ablation (not a shipped mode): `DIFFCTX_PIT_TRANSFORM=maxnorm` fuses the
    // components on their own score shape, rescaled to a common [0, 1], instead
    // of on distributional position. It is the control that isolates the
    // transform, because a linear rescale leaves every downstream
    // magnitude-reading rule invariant: `r_cap = median + sigma*std` scales with
    // the data, so `rel / r_cap` is unchanged. The percentile does not have that
    // property, which is the whole point of comparing them.
    if std::env::var("DIFFCTX_PIT_TRANSFORM").as_deref() == Ok("maxnorm") {
        let denom = ranked
            .iter()
            .map(|(_, s)| *s)
            .fold(0.0f64, f64::max)
            .max(f64::MIN_POSITIVE);
        for (fid, score) in &ranked {
            if admitted.contains(*fid) {
                out.insert((*fid).clone(), *score / denom);
            }
        }
    } else {
        let mut i = 0;
        while i < n {
            // One run of equal scores shares the mean percentile of the run.
            let mut j = i;
            while j + 1 < n && ranked[j + 1].1.to_bits() == ranked[i].1.to_bits() {
                j += 1;
            }
            // Position 0 is the strongest, so invert: the best fragment gets ~1.0.
            let mean_pos = (i + j) as f64 / 2.0;
            let percentile = 1.0 - mean_pos / n as f64;
            for (fid, _) in &ranked[i..=j] {
                if admitted.contains(*fid) {
                    out.insert((*fid).clone(), percentile);
                }
            }
            i = j + 1;
        }
    }

    // Top-k is drawn from the admitted fragments: the agreement bonus asks
    // "do both components rank this highly", and a fragment a component
    // rejected is not ranked highly by it.
    for (fid, _) in ranked
        .iter()
        .filter(|(fid, _)| admitted.contains(*fid))
        .take(top_k)
    {
        top.insert((*fid).clone());
    }
    (out, top)
}

/// Map a fused ranking back onto the reference component's own score
/// distribution, preserving order.
///
/// Selection does not read `rel` as an ordering alone. `compute_r_cap` takes
/// `median + sigma*std` of the score cloud and the utility uses
/// `(rel / r_cap).min(1.0)`, so the *shape* of the distribution decides how many
/// candidates saturate. EGO's raw scores are strongly right-skewed (hop decay
/// puts most mass near zero), which makes `r_cap` small and the saturation
/// meaningful. A percentile is uniform on [0, 1] by construction: its median is
/// ~0.5 and `median + 2*std` lands above the maximum, so nothing saturates and
/// the selector runs in a regime nothing was calibrated for.
///
/// That is a unit mismatch, not a tuning problem, so the fix is to restore the
/// units rather than to re-tune `r_cap_sigma` and `tau` per transform. Fusion
/// then decides the order — which is what fusion is for — and the selector sees
/// the score cloud it was built against.
///
/// A consequence worth stating because it doubles as the correctness gate: at
/// `blend = 1.0` with no agreement bonus the fused order is EGO's order over
/// EGO's own admitted set, so this maps every fragment back to its exact EGO
/// score and the mode must reproduce EGO bit-for-bit.
fn quantile_map_to(
    reference: &[f64],
    fused: &FxHashMap<FragmentId, f64>,
) -> FxHashMap<FragmentId, f64> {
    // A fused score of zero means no component endorsed the fragment.
    // Mapping it anyway would hand it a positive reference value and resurrect
    // a candidate `filter_positive_relevance` is required to drop — at
    // blend=1.0 that alone broke the EGO-equivalence gate (390 vs 371).
    let mut order: Vec<(&FragmentId, f64)> = fused
        .iter()
        .filter(|(_, s)| **s > 0.0)
        .map(|(f, s)| (f, *s))
        .collect();
    if reference.is_empty() || order.is_empty() {
        return order.into_iter().map(|(f, s)| (f.clone(), s)).collect();
    }
    order.sort_by(|(ida, sa), (idb, sb)| sb.total_cmp(sa).then_with(|| ida.cmp(idb)));

    let m = order.len();
    let n = reference.len();
    let mut out = FxHashMap::default();
    let mut i = 0;
    while i < m {
        // A run of equal fused scores is a tie the fusion never resolved, so
        // the run shares one mapped value — its midpoint slot — rather than
        // being fanned across adjacent reference values by id order.
        let mut j = i;
        while j + 1 < m && order[j + 1].1.to_bits() == order[i].1.to_bits() {
            j += 1;
        }
        let mid_rank = (i + j) as f64 / 2.0;
        let idx = if m == 1 {
            n - 1
        } else {
            let pos = ((m - 1) as f64 - mid_rank) / (m - 1) as f64;
            ((pos * (n - 1) as f64).round() as usize).min(n - 1)
        };
        for (fid, _) in &order[i..=j] {
            out.insert((*fid).clone(), reference[idx]);
        }
        i = j + 1;
    }
    out
}

impl ScoringStrategy for PitFusionScoring {
    fn score_and_filter(
        &self,
        all_fragments: &[Fragment],
        core_ids: &FxHashSet<FragmentId>,
        hunks: &[DiffHunk],
        repo_root: Option<&Path>,
        seed_weights: Option<&FxHashMap<FragmentId, f64>>,
        discovered_paths: Option<&FxHashSet<Arc<str>>>,
        deadline: crate::deadline::Deadline,
    ) -> ScoringResult {
        let ego = EgoGraphScoring::new(self.ego_depth).score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
            deadline,
        );
        let lexical = BM25Scoring.score_and_filter(
            all_fragments,
            core_ids,
            hunks,
            repo_root,
            seed_weights,
            discovered_paths,
            deadline,
        );

        let ego_admitted: FxHashSet<FragmentId> = ego
            .filtered_fragments
            .iter()
            .map(|f| f.id.clone())
            .collect();
        let lexical_admitted: FxHashSet<FragmentId> = lexical
            .filtered_fragments
            .iter()
            .map(|f| f.id.clone())
            .collect();

        let (ego_pct, ego_top) = percentiles(
            &ego.rel_scores,
            &ego_admitted,
            core_ids,
            self.agreement_top_k,
        );
        let (lex_pct, lex_top) = percentiles(
            &lexical.rel_scores,
            &lexical_admitted,
            core_ids,
            self.agreement_top_k,
        );

        let mut rel_scores: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for fid in ego_pct.keys().chain(lex_pct.keys()) {
            if rel_scores.contains_key(fid) {
                continue;
            }
            // A fragment only one component admitted contributes 0 from the
            // other — that is the point. Under RRF an absent component was
            // simply silent; here it is an explicit "this signal ranks you at
            // the bottom", which is what stops one weak opinion carrying a
            // fragment.
            let e = ego_pct.get(fid).copied().unwrap_or(0.0);
            let l = lex_pct.get(fid).copied().unwrap_or(0.0);
            let mut score = self.blend * e + (1.0 - self.blend) * l;
            if ego_top.contains(fid) && lex_top.contains(fid) {
                score += self.agreement_bonus;
            }
            rel_scores.insert(fid.clone(), score);
        }

        // `DIFFCTX_PIT_SHAPE=flat` keeps the fused percentiles as the scores the
        // selector sees. That is the pre-`quantile_map_to` behaviour, retained
        // so the transform's cost stays measurable rather than only argued.
        if std::env::var("DIFFCTX_PIT_SHAPE").as_deref() == Ok("flat") {
            let max_fused = rel_scores.values().copied().fold(0.0f64, f64::max);
            if max_fused > 0.0 {
                for v in rel_scores.values_mut() {
                    *v /= max_fused;
                }
            }
            for fid in core_ids {
                rel_scores.insert(fid.clone(), 1.0);
            }
        } else {
            let mut reference: Vec<f64> = ego
                .rel_scores
                .iter()
                .filter(|(fid, s)| {
                    **s > 0.0 && !core_ids.contains(*fid) && ego_admitted.contains(*fid)
                })
                .map(|(_, s)| *s)
                .collect();
            reference.sort_by(f64::total_cmp);
            rel_scores = quantile_map_to(&reference, &rel_scores);

            // Cores keep EGO's own values rather than a pinned 1.0. `r_cap`
            // excludes cores, but the utility does not, and a synthetic 1.0 on
            // EGO's raw scale is a different number from the one EGO assigns.
            for fid in core_ids {
                if let Some(s) = ego.rel_scores.get(fid) {
                    rel_scores.insert(fid.clone(), *s);
                }
            }
        }

        let union_ids: FxHashSet<FragmentId> =
            ego_admitted.union(&lexical_admitted).cloned().collect();
        let union: Vec<Fragment> = all_fragments
            .iter()
            .filter(|f| union_ids.contains(&f.id))
            .cloned()
            .collect();
        let filtered = finish_scoring(&union, core_ids, &rel_scores, &ego.graph);

        ScoringResult {
            // The fusion inherits the ego component's naming-reachability set:
            // admission is a graph property, and the fusion's structural half
            // IS that graph. Leaving this None silently ran fusion arms
            // without the gate.
            admissible_files: ego.admissible_files.clone(),
            rel_scores,
            filtered_fragments: filtered,
            graph: ego.graph,
            graph_build_ms: ego.graph_build_ms,
            ..Default::default()
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

    /// Every scored fragment counts as admitted, which is the pre-#125-fix
    /// behaviour these properties were written against. Tests that care about
    /// the admission gate itself build the ballot explicitly.
    fn ballot(rel: &FxHashMap<FragmentId, f64>) -> FxHashSet<FragmentId> {
        rel.keys().cloned().collect()
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

        let fused = fuse_reciprocal_ranks(
            &[(&ego, &ballot(&ego)), (&lexical, &ballot(&lexical))],
            &cores,
            60.0,
        );
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
            fuse_reciprocal_ranks(
                &[(&modest, &ballot(&modest)), (&lexical, &ballot(&lexical))],
                &cores,
                60.0
            ),
            fuse_reciprocal_ranks(
                &[
                    (&enormous, &ballot(&enormous)),
                    (&lexical, &ballot(&lexical))
                ],
                &cores,
                60.0
            ),
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

        let fused = fuse_reciprocal_ranks(
            &[(&ego, &ballot(&ego)), (&lexical, &ballot(&lexical))],
            &cores,
            60.0,
        );
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
            rank_positions(&with_core, &ballot(&with_core), &cores).get(&ctx),
            rank_positions(&without_core, &ballot(&without_core), &cores).get(&ctx),
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

        let baseline = rank_positions(&tied, &ballot(&tied), &cores);
        for _ in 0..8 {
            let again: FxHashMap<FragmentId, f64> =
                ids.iter().rev().map(|i| (i.clone(), 0.5)).collect();
            assert_eq!(rank_positions(&again, &ballot(&again), &cores), baseline);
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

        let ranks = rank_positions(&component, &ballot(&component), &cores);
        assert!(ranks.contains_key(&kept));
        assert!(
            !ranks.contains_key(&zero),
            "a zero-scored fragment was ranked"
        );

        let fused = fuse_reciprocal_ranks(&[(&component, &ballot(&component))], &cores, 60.0);
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

        let sharp = fuse_reciprocal_ranks(&[(&component, &ballot(&component))], &cores, 1.0);
        let flat = fuse_reciprocal_ranks(&[(&component, &ballot(&component))], &cores, 60.0);

        // Both are max-normalised, so compare the runner-up's share of the top.
        assert!(
            flat[&second] > sharp[&second],
            "k did not flatten adjacent ranks: {} vs {}",
            flat[&second],
            sharp[&second]
        );
    }

    /// The gate that ranks cannot express. A component scores far more
    /// fragments than it admits — BM25 gives anything sharing a generic token a
    /// small positive score — and a rank list is purely ordinal, so the moment a
    /// rejected fragment appears in it the reciprocal rank hands it real fused
    /// mass. Fusing whole score maps instead of the returned result lists cost
    /// 97 oracle cases on precision.
    #[test]
    fn a_component_cannot_vote_for_what_its_own_filters_rejected() {
        let good = fid("good.rs", 1);
        let rejected = fid("garbage.rs", 1);
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        // The rejected fragment outscores the admitted one, so if it is ranked
        // at all it takes rank 1 and the top of the normalised scale with it.
        let component = scores(&[(rejected.clone(), 0.9), (good.clone(), 0.1)]);
        let admitted: FxHashSet<FragmentId> = std::iter::once(good.clone()).collect();

        let fused = fuse_reciprocal_ranks(&[(&component, &admitted)], &cores, 60.0);

        assert!(
            !fused.contains_key(&rejected),
            "a fragment the component filtered out still earned fused mass {:?}",
            fused.get(&rejected)
        );
        assert!(
            fused.contains_key(&good),
            "the admitted fragment lost its vote"
        );
    }

    #[test]
    fn a_percentile_is_a_position_in_the_full_population_not_the_admitted_subset() {
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        // Nine strong fragments the component scored but did not admit, plus
        // one weak admitted straggler. Its percentile must say "bottom of the
        // component's world", not "top of the admitted set of one".
        let mut entries: Vec<(FragmentId, f64)> = (0..9)
            .map(|i| (fid("strong.rs", i + 1), 1.0 - i as f64 * 0.05))
            .collect();
        let weak = fid("weak.rs", 100);
        entries.push((weak.clone(), 0.01));
        let rel = scores(&entries);
        let admitted: FxHashSet<FragmentId> = std::iter::once(weak.clone()).collect();

        let (pct, _) = percentiles(&rel, &admitted, &cores, 5);

        assert_eq!(pct.len(), 1, "only admitted fragments may receive a value");
        let p = pct[&weak];
        assert!(
            p <= 0.2,
            "the weakest of ten scored {p}, reading as strong because the CDF \
             was estimated over the admitted subset"
        );
    }

    #[test]
    fn a_rejected_fragment_gets_no_percentile_at_all() {
        let cores: FxHashSet<FragmentId> = FxHashSet::default();
        let good = fid("good.rs", 1);
        let rejected = fid("garbage.rs", 1);
        let rel = scores(&[(rejected.clone(), 0.9), (good.clone(), 0.1)]);
        let admitted: FxHashSet<FragmentId> = std::iter::once(good.clone()).collect();

        let (pct, top) = percentiles(&rel, &admitted, &cores, 5);

        assert!(
            !pct.contains_key(&rejected),
            "the component's veto was lost"
        );
        assert!(
            !top.contains(&rejected),
            "a rejected fragment cannot sit in the component's top-k"
        );
        assert!(pct.contains_key(&good));
    }

    #[test]
    fn quantile_map_restores_the_reference_distribution_in_fused_order() {
        // Reference: a skewed cloud like EGO's (mass near zero).
        let reference = vec![0.01, 0.02, 0.05, 0.4, 1.9];
        let a = fid("a.rs", 1);
        let b = fid("b.rs", 1);
        let c = fid("c.rs", 1);
        // Fused scores are uniform-ish percentiles; only their order may
        // survive the mapping.
        let fused = scores(&[(a.clone(), 0.9), (b.clone(), 0.5), (c.clone(), 0.1)]);

        let mapped = quantile_map_to(&reference, &fused);

        assert_eq!(mapped[&a], 1.9, "the fused top must take the reference max");
        assert_eq!(
            mapped[&c], 0.01,
            "the fused bottom must take the reference min"
        );
        assert!(
            mapped[&a] > mapped[&b] && mapped[&b] > mapped[&c],
            "the fused order was not preserved"
        );
    }

    #[test]
    fn quantile_map_over_the_same_population_is_the_identity_on_values() {
        // blend=1.0, bonus=0: the fused order IS ego's order over ego's own
        // admitted set, so mapping back onto ego's sorted scores must return
        // exactly those scores — the property the corpus gate checks end to end.
        let ids: Vec<FragmentId> = (0..5).map(|i| fid("f.rs", i + 1)).collect();
        let ego_scores = [0.02, 0.07, 0.11, 0.55, 0.9];
        let mut reference: Vec<f64> = ego_scores.to_vec();
        reference.sort_by(f64::total_cmp);
        // Fused percentiles in the same order as the ego scores.
        let fused = scores(
            &ids.iter()
                .zip([0.2, 0.4, 0.6, 0.8, 1.0])
                .map(|(id, p)| (id.clone(), p))
                .collect::<Vec<_>>(),
        );

        let mapped = quantile_map_to(&reference, &fused);

        for (id, expected) in ids.iter().zip(ego_scores) {
            assert_eq!(
                mapped[id], expected,
                "same-population quantile map must reproduce the component's own values"
            );
        }
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
            let result = strategy.score_and_filter(
                &empty,
                &FxHashSet::default(),
                &[],
                None,
                None,
                None,
                crate::deadline::Deadline::none(),
            );
            assert!(
                result.filtered_fragments.is_empty(),
                "{mode:?} invented fragments from an empty universe"
            );
        }
        // Keeps `FragmentKind` in scope for the helper above.
        let _ = FragmentKind::Function;
    }
}
