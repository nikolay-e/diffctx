use std::path::Path;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::LEXICAL;
use crate::config::tokenization::TOKENIZATION;
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

/// Maps each unique term to a compact u32 id. Stores each term string exactly once.
struct TermInterner {
    by_str: FxHashMap<String, u32>,
}

impl TermInterner {
    fn new() -> Self {
        Self {
            by_str: FxHashMap::default(),
        }
    }

    fn intern(&mut self, term: String) -> u32 {
        let next_id = self.by_str.len() as u32;
        *self.by_str.entry(term).or_insert(next_id)
    }

    fn len(&self) -> usize {
        self.by_str.len()
    }
}

impl LexicalEdgeBuilder {
    /// Tokenize and filter identifiers for one fragment. Returns the raw filtered identifier list.
    fn tokens(frag: &Fragment) -> Vec<String> {
        let profile = profile_from_path(frag.path());
        let idents =
            extract_identifier_list(&frag.content, TOKENIZATION.query_min_identifier_length);
        filter_idents(&idents, 3, profile)
    }
}

impl EdgeBuilder for LexicalEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        if fragments.is_empty() {
            return FxHashMap::default();
        }

        let n_docs = fragments.len();
        let max_df = (n_docs as f64 * LEXICAL.max_df_ratio).max(1.0) as usize;

        // Pass 1: tokenize each fragment in parallel; flatten to per-fragment Vec<String>.
        let per_frag_tokens: Vec<Vec<String>> =
            fragments.par_iter().map(|f| Self::tokens(f)).collect();

        // Pass 2: build the term interner serially, computing document frequency in one go.
        let mut interner = TermInterner::new();
        let mut doc_freq: Vec<u32> = Vec::new();
        let per_frag_term_ids: Vec<Vec<u32>> = per_frag_tokens
            .into_iter()
            .map(|tokens| {
                let mut seen_in_doc: FxHashSet<u32> = FxHashSet::default();
                let mut ids: Vec<u32> = Vec::with_capacity(tokens.len());
                for tok in tokens {
                    let id = interner.intern(tok);
                    if doc_freq.len() <= id as usize {
                        doc_freq.resize(id as usize + 1, 0);
                    }
                    if seen_in_doc.insert(id) {
                        doc_freq[id as usize] += 1;
                    }
                    ids.push(id);
                }
                ids
            })
            .collect();

        let n_terms = interner.len();
        // Interner string-table is no longer needed once doc_freq has been built.
        drop(interner);

        let n_docs_f = n_docs as f64;
        let mut idf: Vec<f32> = Vec::with_capacity(n_terms);
        for &df in &doc_freq {
            let v = ((n_docs_f + 1.0) / (df as f64 + 1.0)).ln() + 1.0;
            idf.push(v as f32);
        }

        // Pass 3: build TF-IDF vectors as sparse Vec<(TermId, f32)>, normalized.
        let tf_idf: Vec<Vec<(u32, f32)>> = per_frag_term_ids
            .par_iter()
            .map(|term_ids| {
                let mut tf: FxHashMap<u32, u32> = FxHashMap::default();
                for &id in term_ids {
                    *tf.entry(id).or_insert(0) += 1;
                }
                let mut vec: Vec<(u32, f32)> = Vec::with_capacity(tf.len());
                for (&term_id, &count) in &tf {
                    let df = doc_freq[term_id as usize] as usize;
                    if df == 0 || df > max_df {
                        continue;
                    }
                    let term_idf = idf[term_id as usize];
                    if (term_idf as f64) < LEXICAL.min_idf {
                        continue;
                    }
                    vec.push((term_id, count as f32 * term_idf));
                }
                let norm: f32 = vec.iter().map(|(_, w)| w * w).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for (_, w) in &mut vec {
                        *w /= norm;
                    }
                }
                vec.sort_unstable_by_key(|&(id, _)| id);
                vec
            })
            .collect();

        drop(per_frag_term_ids);
        drop(doc_freq);
        drop(idf);

        // Pass 4: invert into postings — for each term, list of (frag_idx, weight).
        // Consume tf_idf as we go so it never coexists with the inverted index.
        let mut postings: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n_terms];
        for (frag_idx, vec) in tf_idf.into_iter().enumerate() {
            for (term_id, weight) in vec {
                postings[term_id as usize].push((frag_idx as u32, weight));
            }
        }

        // Pass 5: O(F²) inner loop over each posting, capped by max_postings.
        // Drop each posting list as soon as we are done with it.
        let mut dot_products: FxHashMap<(u32, u32), f32> = FxHashMap::default();
        for posting_list in postings.iter_mut() {
            if posting_list.len() > LEXICAL.max_postings || posting_list.len() < 2 {
                posting_list.clear();
                posting_list.shrink_to_fit();
                continue;
            }
            for i in 0..posting_list.len() {
                let (frag_i, weight_i) = posting_list[i];
                for j in (i + 1)..posting_list.len() {
                    let (frag_j, weight_j) = posting_list[j];
                    let pair = if frag_i < frag_j {
                        (frag_i, frag_j)
                    } else {
                        (frag_j, frag_i)
                    };
                    *dot_products.entry(pair).or_insert(0.0) += weight_i * weight_j;
                }
            }
            posting_list.clear();
            posting_list.shrink_to_fit();
        }
        drop(postings);

        // Pass 6: turn pairwise similarities into per-node top-k candidate edges.
        let frag_paths: Vec<&str> = fragments.iter().map(|f| f.path()).collect();
        let mut neighbors_by_node: FxHashMap<u32, Vec<(f32, u32)>> = FxHashMap::default();

        let min_sim = LEXICAL.min_similarity as f32;
        let backward_factor = LEXICAL.backward_factor as f32;
        for ((src_idx, dst_idx), sim) in &dot_products {
            if *sim < min_sim {
                continue;
            }
            let src_path = Path::new(frag_paths[*src_idx as usize]);
            let dst_path = Path::new(frag_paths[*dst_idx as usize]);
            let fwd = clamp_lexical_weight(*sim as f64, Some(src_path), Some(dst_path)) as f32;
            let bwd = clamp_lexical_weight(*sim as f64, Some(dst_path), Some(src_path)) as f32
                * backward_factor;
            neighbors_by_node
                .entry(*src_idx)
                .or_default()
                .push((fwd, *dst_idx));
            neighbors_by_node
                .entry(*dst_idx)
                .or_default()
                .push((bwd, *src_idx));
        }

        let frag_ids: Vec<&FragmentId> = fragments.iter().map(|f| &f.id).collect();
        let mut edges: EdgeDict = FxHashMap::default();
        for (node_idx, mut candidates) in neighbors_by_node {
            candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            candidates.truncate(LEXICAL.top_k_neighbors);
            for (weight, neighbor_idx) in candidates {
                let key = (
                    frag_ids[node_idx as usize].clone(),
                    frag_ids[neighbor_idx as usize].clone(),
                );
                let existing = edges.get(&key).copied().unwrap_or(0.0);
                let weight_f64 = weight as f64;
                if weight_f64 > existing {
                    edges.insert(key, weight_f64);
                }
            }
        }

        edges
    }

    fn is_expensive(&self) -> bool {
        true
    }
}
