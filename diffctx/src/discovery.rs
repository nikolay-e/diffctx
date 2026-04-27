use std::borrow::Cow;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::bm25::BM25;
use crate::config::tokenization::TOKENIZATION;
use crate::types::extract_identifier_list;

pub struct DiscoveryContext {
    pub root_dir: PathBuf,
    pub changed_files: Vec<PathBuf>,
    pub all_candidates: Vec<PathBuf>,
    pub diff_text: String,
    pub expansion_concepts: FxHashSet<String>,
    pub file_cache: FxHashMap<PathBuf, String>,
}

impl DiscoveryContext {
    pub fn read_file(&self, path: &Path) -> Option<Cow<'_, str>> {
        if let Some(content) = self.file_cache.get(path) {
            return Some(Cow::Borrowed(content.as_str()));
        }
        std::fs::read_to_string(path).ok().map(Cow::Owned)
    }
}

pub trait DiscoveryStrategy: Send + Sync {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf>;
}

pub struct DefaultDiscovery;

impl DiscoveryStrategy for DefaultDiscovery {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();

        let mut discovered = crate::edges::discover_all_related_files(
            &ctx.changed_files,
            &ctx.all_candidates,
            Some(ctx.root_dir.as_path()),
            Some(&ctx.file_cache),
        );
        discovered.retain(|p| !changed_set.contains(p.as_path()));

        let rare_files = expand_by_rare_identifiers(ctx, &changed_set);
        let existing: FxHashSet<PathBuf> = discovered.iter().cloned().collect();
        for f in rare_files {
            if !existing.contains(&f) {
                discovered.push(f);
            }
        }

        discovered
    }
}

fn expand_by_rare_identifiers(
    ctx: &DiscoveryContext,
    changed_set: &FxHashSet<&Path>,
) -> Vec<PathBuf> {
    let rare_threshold = crate::config::limits::LIMITS.rare_identifier_threshold;

    let mut ident_to_files: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
    for f in &ctx.all_candidates {
        if changed_set.contains(f.as_path()) {
            continue;
        }
        if let Some(content) = ctx.read_file(f) {
            let idents: FxHashSet<String> = crate::types::extract_identifiers(
                &content,
                TOKENIZATION.query_min_identifier_length,
            );
            for ident in &ctx.expansion_concepts {
                if idents.contains(ident) {
                    ident_to_files
                        .entry(ident.clone())
                        .or_default()
                        .push(f.clone());
                }
            }
        }
    }

    let mut result: Vec<PathBuf> = Vec::new();
    let mut seen: FxHashSet<PathBuf> = FxHashSet::default();
    for (_ident, files) in &ident_to_files {
        if files.len() <= rare_threshold {
            for f in files {
                if seen.insert(f.clone()) {
                    result.push(f.clone());
                }
            }
        }
    }
    result
}

pub struct TestFileDiscovery;

const TEST_PREFIXES: &[&str] = &["test_", "spec_"];
const TEST_SUFFIXES: &[&str] = &["_test", "_spec", ".test", ".spec", "-test", "-spec"];

impl DiscoveryStrategy for TestFileDiscovery {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();
        let mut target_stems: FxHashSet<String> = FxHashSet::default();

        for f in &ctx.changed_files {
            let stem = f
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if TEST_PREFIXES.iter().any(|p| stem.starts_with(p)) {
                continue;
            }
            if TEST_SUFFIXES.iter().any(|s| stem.ends_with(s)) {
                continue;
            }
            target_stems.insert(stem.clone());
            for prefix in TEST_PREFIXES {
                target_stems.insert(format!("{}{}", prefix, stem));
            }
            for suffix in TEST_SUFFIXES {
                target_stems.insert(format!("{}{}", stem, suffix));
            }
        }

        let mut discovered: Vec<PathBuf> = Vec::new();
        for candidate in &ctx.all_candidates {
            if changed_set.contains(candidate.as_path()) {
                continue;
            }
            let stem = candidate
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if target_stems.contains(&stem) {
                discovered.push(candidate.clone());
            }
        }
        discovered
    }
}

pub struct BM25Discovery {
    pub top_k: usize,
}

impl BM25Discovery {
    pub fn new(top_k: usize) -> Self {
        Self { top_k }
    }

    fn bm25_score(
        doc: &[String],
        query_set: &FxHashSet<String>,
        idf: &FxHashMap<String, f64>,
        avgdl: f64,
    ) -> f64 {
        let dl = doc.len() as f64;
        let mut tf: FxHashMap<&str, u32> = FxHashMap::default();
        for t in doc {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let mut s = 0.0;
        for t in query_set {
            let freq = tf.get(t.as_str()).copied().unwrap_or(0) as f64;
            if freq == 0.0 {
                continue;
            }
            let idf_val = idf.get(t).copied().unwrap_or(0.0);
            s += idf_val * (freq * BM25.k1)
                / (freq + BM25.doc_len_factor * (1.0 - BM25.b + BM25.b * dl / avgdl));
        }
        s
    }
}

impl DiscoveryStrategy for BM25Discovery {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let query_tokens = extract_identifier_list(&ctx.diff_text, BM25.min_query_token_length);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();
        let mut corpus: Vec<Vec<String>> = Vec::new();
        let mut paths: Vec<PathBuf> = Vec::new();

        for f in &ctx.all_candidates {
            if changed_set.contains(f.as_path()) {
                continue;
            }
            let content = match ctx.read_file(f) {
                Some(c) => c,
                None => continue,
            };
            corpus.push(extract_identifier_list(
                &content,
                BM25.min_query_token_length,
            ));
            paths.push(f.clone());
        }

        if corpus.is_empty() {
            return Vec::new();
        }

        let n_docs = corpus.len();
        let avgdl = corpus.iter().map(|d| d.len()).sum::<usize>() as f64 / n_docs as f64;

        let mut df: FxHashMap<String, usize> = FxHashMap::default();
        for doc in &corpus {
            let unique: FxHashSet<&str> = doc.iter().map(|s| s.as_str()).collect();
            for term in unique {
                *df.entry(term.to_string()).or_insert(0) += 1;
            }
        }

        let query_set: FxHashSet<String> = query_tokens.into_iter().collect();
        let idf: FxHashMap<String, f64> = query_set
            .iter()
            .map(|t| {
                let d = df.get(t).copied().unwrap_or(0) as f64;
                let val =
                    ((n_docs as f64 - d + BM25.idf_smoothing) / (d + BM25.idf_smoothing)).ln_1p();
                (t.clone(), val)
            })
            .collect();

        let scores: Vec<f64> = corpus
            .par_iter()
            .map(|doc| Self::bm25_score(doc, &query_set, &idf, avgdl))
            .collect();

        let mut ranked: Vec<usize> = (0..scores.len()).collect();
        ranked.sort_by(|&a, &b| {
            scores[b]
                .partial_cmp(&scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        ranked
            .into_iter()
            .take(self.top_k)
            .filter(|&i| scores[i] > 0.0)
            .map(|i| paths[i].clone())
            .collect()
    }
}

pub struct EnsembleDiscovery {
    strategies: Vec<Box<dyn DiscoveryStrategy>>,
}

impl EnsembleDiscovery {
    pub fn new(strategies: Vec<Box<dyn DiscoveryStrategy>>) -> Self {
        Self { strategies }
    }

    pub fn default_ensemble() -> Self {
        Self {
            strategies: vec![
                Box::new(DefaultDiscovery),
                Box::new(TestFileDiscovery),
                Box::new(BM25Discovery::new(1)),
            ],
        }
    }
}

impl DiscoveryStrategy for EnsembleDiscovery {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let per_strategy: Vec<Vec<PathBuf>> = self
            .strategies
            .par_iter()
            .map(|strategy| strategy.discover(ctx))
            .collect();

        let mut seen: FxHashSet<PathBuf> = FxHashSet::default();
        let mut result: Vec<PathBuf> = Vec::new();
        for paths in per_strategy {
            for path in paths {
                if seen.insert(path.clone()) {
                    result.push(path);
                }
            }
        }

        result
    }
}
