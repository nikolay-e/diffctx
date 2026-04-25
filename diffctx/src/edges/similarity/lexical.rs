use std::path::Path;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::LEXICAL;
use crate::config::weights::{DEFAULT_LANG_WEIGHTS, LANG_WEIGHTS, LangWeights};
use crate::languages::EXTENSION_TO_LANGUAGE;
use crate::stopwords::{filter_idents, profile_from_path};
use crate::types::{Fragment, FragmentId, extract_identifier_list};

use super::super::EdgeDict;
use super::super::base::EdgeBuilder;

static LANG_ALIAS: &[(&str, &str)] = &[
    ("bash", "shell"),
    ("zsh", "shell"),
    ("fish", "shell"),
    ("powershell", "shell"),
];

fn get_lang_weights(path: &Path) -> &LangWeights {
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let lang = EXTENSION_TO_LANGUAGE.get(ext.as_str()).copied();
    if let Some(lang) = lang {
        let aliased = LANG_ALIAS
            .iter()
            .find(|(k, _)| *k == lang)
            .map(|(_, v)| *v)
            .unwrap_or(lang);
        LANG_WEIGHTS.get(aliased).unwrap_or(&DEFAULT_LANG_WEIGHTS)
    } else {
        &DEFAULT_LANG_WEIGHTS
    }
}

fn clamp_lexical_weight(raw_sim: f64, src_path: Option<&Path>, dst_path: Option<&Path>) -> f64 {
    let (lex_max, lex_min) = match (src_path, dst_path) {
        (Some(sp), Some(dp)) => {
            let sw = get_lang_weights(sp);
            let dw = get_lang_weights(dp);
            (
                sw.lexical_max.max(dw.lexical_max),
                sw.lexical_min.max(dw.lexical_min),
            )
        }
        _ => (LEXICAL.weight_max, LEXICAL.weight_min),
    };

    if raw_sim < LEXICAL.min_similarity {
        return 0.0;
    }
    let denom = 1.0 - LEXICAL.min_similarity;
    if denom <= 0.0 {
        return lex_max;
    }
    let normalized = (raw_sim - LEXICAL.min_similarity) / denom;
    lex_min + normalized * (lex_max - lex_min)
}

pub struct LexicalEdgeBuilder;

impl LexicalEdgeBuilder {
    fn compute_doc_frequencies(&self, fragments: &[Fragment]) -> FxHashMap<String, usize> {
        let mut doc_freq: FxHashMap<String, usize> = FxHashMap::default();
        for frag in fragments {
            let profile = profile_from_path(frag.path());
            let idents = extract_identifier_list(&frag.content, 3);
            let filtered = filter_idents(&idents, 3, profile);
            let mut seen: FxHashSet<String> = FxHashSet::default();
            for ident in filtered {
                if seen.insert(ident.clone()) {
                    *doc_freq.entry(ident).or_insert(0) += 1;
                }
            }
        }
        doc_freq
    }

    fn compute_idf(
        &self,
        doc_freq: &FxHashMap<String, usize>,
        n_docs: usize,
    ) -> FxHashMap<String, f64> {
        doc_freq
            .iter()
            .map(|(term, &df)| {
                let idf = ((n_docs as f64 + 1.0) / (df as f64 + 1.0)).ln() + 1.0;
                (term.clone(), idf)
            })
            .collect()
    }

    fn build_tf_idf_vector(
        &self,
        frag: &Fragment,
        doc_freq: &FxHashMap<String, usize>,
        idf: &FxHashMap<String, f64>,
        max_df: usize,
    ) -> FxHashMap<String, f64> {
        let profile = profile_from_path(frag.path());
        let idents = extract_identifier_list(&frag.content, 3);
        let filtered = filter_idents(&idents, 3, profile);

        let mut tf: FxHashMap<String, usize> = FxHashMap::default();
        for ident in filtered {
            *tf.entry(ident).or_insert(0) += 1;
        }

        let mut vec: FxHashMap<String, f64> = FxHashMap::default();
        for (term, &count) in &tf {
            let df = doc_freq.get(term).copied().unwrap_or(0);
            if df == 0 || df > max_df {
                continue;
            }
            let term_idf = idf.get(term).copied().unwrap_or(1.0);
            if term_idf < LEXICAL.min_idf {
                continue;
            }
            vec.insert(term.clone(), count as f64 * term_idf);
        }

        let norm: f64 = vec.values().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 0.0 {
            for v in vec.values_mut() {
                *v /= norm;
            }
        }

        vec
    }
}

impl EdgeBuilder for LexicalEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        if fragments.is_empty() {
            return FxHashMap::default();
        }

        let doc_freq = self.compute_doc_frequencies(fragments);
        let n_docs = fragments.len();
        let max_df = (n_docs as f64 * LEXICAL.max_df_ratio).max(1.0) as usize;
        let idf = self.compute_idf(&doc_freq, n_docs);

        let tf_idf_vectors: FxHashMap<FragmentId, FxHashMap<String, f64>> = fragments
            .par_iter()
            .map(|frag| {
                let vec = self.build_tf_idf_vector(frag, &doc_freq, &idf, max_df);
                (frag.id.clone(), vec)
            })
            .collect();

        let mut postings: FxHashMap<String, Vec<(FragmentId, f64)>> = FxHashMap::default();
        for (frag_id, vec) in &tf_idf_vectors {
            for (term, &weight) in vec {
                postings
                    .entry(term.clone())
                    .or_default()
                    .push((frag_id.clone(), weight));
            }
        }

        let mut dot_products: FxHashMap<(FragmentId, FragmentId), f64> = FxHashMap::default();
        for (_term, posting_list) in &postings {
            if posting_list.len() > LEXICAL.max_postings {
                continue;
            }
            for i in 0..posting_list.len() {
                let (ref frag_i, weight_i) = posting_list[i];
                for j in (i + 1)..posting_list.len() {
                    let (ref frag_j, weight_j) = posting_list[j];
                    let pair = if frag_i.to_string() < frag_j.to_string() {
                        (frag_i.clone(), frag_j.clone())
                    } else {
                        (frag_j.clone(), frag_i.clone())
                    };
                    *dot_products.entry(pair).or_insert(0.0) += weight_i * weight_j;
                }
            }
        }

        let id_to_path: FxHashMap<FragmentId, &str> =
            fragments.iter().map(|f| (f.id.clone(), f.path())).collect();

        let mut neighbors_by_node: FxHashMap<FragmentId, Vec<(f64, FragmentId)>> =
            FxHashMap::default();

        for ((src, dst), sim) in &dot_products {
            if *sim < LEXICAL.min_similarity {
                continue;
            }
            let src_path = id_to_path.get(src).map(|s| Path::new(*s));
            let dst_path = id_to_path.get(dst).map(|s| Path::new(*s));
            let fwd = clamp_lexical_weight(*sim, src_path, dst_path);
            let bwd = clamp_lexical_weight(*sim, dst_path, src_path) * LEXICAL.backward_factor;
            neighbors_by_node
                .entry(src.clone())
                .or_default()
                .push((fwd, dst.clone()));
            neighbors_by_node
                .entry(dst.clone())
                .or_default()
                .push((bwd, src.clone()));
        }

        let mut edges: EdgeDict = FxHashMap::default();
        for (_node, mut candidates) in neighbors_by_node {
            candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            candidates.truncate(LEXICAL.top_k_neighbors);
            for (weight, neighbor) in candidates {
                let key = (_node.clone(), neighbor);
                let existing = edges.get(&key).copied().unwrap_or(0.0);
                if weight > existing {
                    edges.insert(key, weight);
                }
            }
        }

        edges
    }

    fn is_expensive(&self) -> bool {
        true
    }
}
