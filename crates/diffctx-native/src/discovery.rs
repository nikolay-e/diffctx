use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::bm25::BM25;
use crate::token_corpus::{DocTokens, TokenCorpus};
use crate::types::extract_identifier_list;

pub struct DiscoveryContext {
    pub root_dir: PathBuf,
    pub changed_files: Vec<PathBuf>,
    pub all_candidates: Vec<PathBuf>,
    pub diff_text: String,
    pub expansion_concepts: FxHashSet<String>,
    pub file_cache: FxHashMap<PathBuf, String>,
    pub token_corpus: OnceLock<TokenCorpus>,
}

impl DiscoveryContext {
    pub fn read_file(&self, path: &Path) -> Option<Cow<'_, str>> {
        if let Some(content) = self.file_cache.get(path) {
            return Some(Cow::Borrowed(content.as_str()));
        }
        std::fs::read_to_string(path).ok().map(Cow::Owned)
    }

    pub fn shared_corpus(&self) -> &TokenCorpus {
        self.token_corpus.get_or_init(|| TokenCorpus::build(self))
    }
}

pub trait DiscoveryStrategy: Send + Sync {
    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf>;

    /// Stable identifier for telemetry. Not a display name — it is written into
    /// the provenance dump and read back by analysis, so changing one renames a
    /// column in every recorded run.
    fn name(&self) -> &'static str;

    /// Discovery plus which strategy surfaced each path.
    ///
    /// The ensemble dedupes first-seen and used to throw the attribution away,
    /// which makes the universe ceiling undiagnosable: "never surfaced at all"
    /// and "surfaced but not selected" are different failures with different
    /// fixes (#130), and neither is visible from the selected set alone. The
    /// default is honest for a single strategy — everything it returns, it
    /// found — and only the ensemble needs to override it.
    fn discover_attributed(
        &self,
        ctx: &DiscoveryContext,
    ) -> (Vec<PathBuf>, Vec<(PathBuf, &'static str)>) {
        let paths = self.discover(ctx);
        let attribution = paths.iter().map(|p| (p.clone(), self.name())).collect();
        (paths, attribution)
    }
}

pub struct DefaultDiscovery;

impl DiscoveryStrategy for DefaultDiscovery {
    fn name(&self) -> &'static str {
        "structural"
    }

    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();

        let mut discovered = crate::edges::discover_all_related_files(
            &ctx.changed_files,
            &ctx.all_candidates,
            Some(ctx.root_dir.as_path()),
            Some(&ctx.file_cache),
        );
        discovered.retain(|p| !changed_set.contains(p.as_path()));

        let rare_files = expand_by_rare_identifiers(ctx);
        let existing: FxHashSet<PathBuf> = discovered.iter().cloned().collect();
        for f in rare_files {
            if !existing.contains(&f) {
                discovered.push(f);
            }
        }

        discovered
    }
}

fn expand_by_rare_identifiers(ctx: &DiscoveryContext) -> Vec<PathBuf> {
    let rare_threshold = crate::config::limits::LIMITS.rare_identifier_threshold;

    let mut ident_to_files: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
    for (path, doc) in &ctx.shared_corpus().docs {
        for ident in &ctx.expansion_concepts {
            if doc.term_counts.contains_key(ident) {
                ident_to_files
                    .entry(ident.clone())
                    .or_default()
                    .push(path.clone());
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

fn lowercase_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

impl DiscoveryStrategy for TestFileDiscovery {
    fn name(&self) -> &'static str {
        "test_pairing"
    }

    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();
        // Test files legitimately live in a tree of their own (`tests/test_x.py`
        // for `src/x.py`), so the prefixed and suffixed stems have to match
        // anywhere.
        let mut test_stems: FxHashSet<String> = FxHashSet::default();
        // The bare stem is a different rule with a different justification:
        // `foo.h` beside `foo.c`, `x.ts` beside `x.js`. What makes such a pair
        // meaningful is co-location, so this one is scoped to the changed
        // file's own directory. Repo-wide it degenerates on exactly the
        // basenames real projects repeat most — one changed `mod.rs`,
        // `index.ts` or `__init__.py` would pull in every namesake in the tree,
        // and the discovery universe bounds every later stage.
        let mut sibling_stems: FxHashMap<PathBuf, FxHashSet<String>> = FxHashMap::default();

        for f in &ctx.changed_files {
            let stem = lowercase_stem(f);
            if TEST_PREFIXES.iter().any(|p| stem.starts_with(p)) {
                continue;
            }
            if TEST_SUFFIXES.iter().any(|s| stem.ends_with(s)) {
                continue;
            }
            sibling_stems
                .entry(f.parent().unwrap_or(Path::new("")).to_path_buf())
                .or_default()
                .insert(stem.clone());
            for prefix in TEST_PREFIXES {
                test_stems.insert(format!("{prefix}{stem}"));
            }
            for suffix in TEST_SUFFIXES {
                test_stems.insert(format!("{stem}{suffix}"));
            }
        }

        let mut discovered: Vec<PathBuf> = Vec::new();
        for candidate in &ctx.all_candidates {
            if changed_set.contains(candidate.as_path()) {
                continue;
            }
            let stem = lowercase_stem(candidate);
            let is_sibling = sibling_stems
                .get(candidate.parent().unwrap_or(Path::new("")))
                .is_some_and(|stems| stems.contains(&stem));
            if test_stems.contains(&stem) || is_sibling {
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
        doc: &DocTokens,
        query_set: &FxHashSet<String>,
        idf: &FxHashMap<String, f64>,
        avgdl: f64,
    ) -> f64 {
        let dl = doc.total_len as f64;
        let mut s = 0.0;
        for t in query_set {
            let freq = doc.term_counts.get(t).copied().unwrap_or(0) as f64;
            if freq == 0.0 {
                continue;
            }
            let idf_val = idf.get(t).copied().unwrap_or(0.0);
            s += idf_val * (freq * BM25.k1)
                / (freq + BM25.k1 * (1.0 - BM25.b + BM25.b * dl / avgdl));
        }
        s
    }
}

impl DiscoveryStrategy for BM25Discovery {
    fn name(&self) -> &'static str {
        "lexical_bm25"
    }

    fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
        let query_tokens = extract_identifier_list(&ctx.diff_text, BM25.min_query_token_length);
        if query_tokens.is_empty() {
            return Vec::new();
        }
        let query_set: FxHashSet<String> = query_tokens.into_iter().collect();

        let pairs = &ctx.shared_corpus().docs;

        if pairs.is_empty() {
            return Vec::new();
        }
        let n_docs = pairs.len();
        if n_docs > 5000 {
            tracing::warn!(
                "BM25Discovery: large candidate corpus ({n_docs} docs) — using inverted-index fast path"
            );
        }

        // Single pass: compute df globally + inverted-index posting lists
        // for query terms only (skip indexing terms not in the query — they
        // are never needed and would balloon memory on large repos).
        let mut df: FxHashMap<String, usize> = FxHashMap::default();
        let mut postings: FxHashMap<String, Vec<usize>> = FxHashMap::default();
        let mut total_len: usize = 0;
        for (doc_id, (_, doc)) in pairs.iter().enumerate() {
            total_len += doc.total_len as usize;
            for term in doc.term_counts.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
                if query_set.contains(term.as_str()) {
                    postings.entry(term.clone()).or_default().push(doc_id);
                }
            }
        }
        let avgdl = total_len as f64 / n_docs as f64;

        let idf: FxHashMap<String, f64> = query_set
            .iter()
            .map(|t| {
                let d = df.get(t).copied().unwrap_or(0) as f64;
                let val =
                    ((n_docs as f64 - d + BM25.idf_smoothing) / (d + BM25.idf_smoothing)).ln_1p();
                (t.clone(), val)
            })
            .collect();

        // Candidate doc-ids = union of posting lists for query terms. Docs
        // not in this set contain zero query terms and would score 0 — skip
        // them. This is the algorithmic win: scoring shrinks from O(N_docs)
        // to O(|posting-list union|), typically ~10-100× smaller on big
        // corpora where the query is sparse against the corpus vocabulary.
        let mut candidate_ids: FxHashSet<usize> = FxHashSet::default();
        for term in &query_set {
            if let Some(p) = postings.get(term) {
                candidate_ids.extend(p);
            }
        }
        if candidate_ids.is_empty() {
            return Vec::new();
        }

        let candidate_vec: Vec<usize> = candidate_ids.into_iter().collect();
        let scored: Vec<(usize, f64)> = candidate_vec
            .par_iter()
            .map(|&doc_id| {
                let s = Self::bm25_score(&pairs[doc_id].1, &query_set, &idf, avgdl);
                (doc_id, s)
            })
            .collect();

        let mut ranked: Vec<(usize, f64)> = scored.into_iter().filter(|(_, s)| *s > 0.0).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        ranked
            .into_iter()
            .take(self.top_k)
            .map(|(i, _)| pairs[i].0.clone())
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
        self.discover_attributed(ctx).0
    }

    fn name(&self) -> &'static str {
        "ensemble"
    }

    /// First-seen dedup, with the winner recorded rather than discarded.
    ///
    /// "First" is strategy order, not merit: a path both the structural and the
    /// lexical strategy would have found is credited to whichever runs earlier.
    /// The attribution answers "could anything have surfaced this", which is the
    /// universe-ceiling question; it is not a claim that the other strategies
    /// would have missed it.
    fn discover_attributed(
        &self,
        ctx: &DiscoveryContext,
    ) -> (Vec<PathBuf>, Vec<(PathBuf, &'static str)>) {
        let per_strategy: Vec<(&'static str, Vec<PathBuf>)> = self
            .strategies
            .par_iter()
            .map(|strategy| (strategy.name(), strategy.discover(ctx)))
            .collect();

        let mut seen: FxHashSet<PathBuf> = FxHashSet::default();
        let mut result: Vec<PathBuf> = Vec::new();
        let mut attribution: Vec<(PathBuf, &'static str)> = Vec::new();
        for (source, paths) in per_strategy {
            for path in paths {
                if seen.insert(path.clone()) {
                    attribution.push((path.clone(), source));
                    result.push(path);
                }
            }
        }

        (result, attribution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> DocTokens {
        let terms = extract_identifier_list(text, 1);
        let total_len = terms.len() as u32;
        let mut term_counts: FxHashMap<String, u32> = FxHashMap::default();
        for t in terms {
            *term_counts.entry(t).or_insert(0) += 1;
        }
        DocTokens {
            term_counts,
            total_len,
        }
    }

    struct CtxBuilder {
        changed: Vec<&'static str>,
        candidates: Vec<&'static str>,
        diff_text: String,
        concepts: Vec<&'static str>,
        docs: Vec<(&'static str, &'static str)>,
    }

    impl CtxBuilder {
        fn new() -> Self {
            Self {
                changed: Vec::new(),
                candidates: Vec::new(),
                diff_text: String::new(),
                concepts: Vec::new(),
                docs: Vec::new(),
            }
        }

        fn build(self) -> DiscoveryContext {
            let root = PathBuf::from("/repo");
            let corpus = TokenCorpus {
                docs: self
                    .docs
                    .iter()
                    .map(|(p, text)| (root.join(p), doc(text)))
                    .collect(),
            };
            let token_corpus = OnceLock::new();
            token_corpus
                .set(corpus)
                .unwrap_or_else(|_| unreachable!("fresh OnceLock"));
            DiscoveryContext {
                root_dir: root.clone(),
                changed_files: self.changed.iter().map(|p| root.join(p)).collect(),
                all_candidates: self.candidates.iter().map(|p| root.join(p)).collect(),
                diff_text: self.diff_text,
                expansion_concepts: self.concepts.iter().map(|s| s.to_string()).collect(),
                file_cache: FxHashMap::default(),
                token_corpus,
            }
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = paths
            .iter()
            .map(|p| {
                p.strip_prefix("/repo")
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        v.sort();
        v
    }

    /// Each naming convention pairs through a different entry in
    /// TEST_PREFIXES/TEST_SUFFIXES, so removing one leaves the others working
    /// and only that ecosystem's coverage silently disappears.
    /// The bare stem of a changed file is also a target, which pairs
    /// `foo.c` with `include/foo.h`. Applied repo-wide it turns the most
    /// ordinary basenames into a fan-out: one changed `mod.rs` drags in every
    /// other `mod.rs` in the tree, and the discovery universe bounds
    /// everything downstream.
    #[test]
    fn bare_stem_pairing_does_not_drag_in_same_named_files_repo_wide() {
        let ctx = CtxBuilder {
            changed: vec!["crates/a/src/mod.rs"],
            candidates: vec![
                "crates/a/src/mod_test.rs",
                "crates/b/src/mod.rs",
                "crates/c/src/mod.rs",
                "vendor/d/mod.rs",
            ],
            ..CtxBuilder::new()
        }
        .build();

        assert_eq!(
            names(&TestFileDiscovery.discover(&ctx)),
            vec!["crates/a/src/mod_test.rs"],
            "unrelated same-basename files entered the universe"
        );
    }

    /// The co-located half of the same rule is the reason it exists: a header
    /// beside its implementation is a genuine pairing.
    #[test]
    fn bare_stem_pairing_still_finds_a_co_located_counterpart() {
        let ctx = CtxBuilder {
            changed: vec!["src/parser.c"],
            candidates: vec!["src/parser.h", "other/parser.h"],
            ..CtxBuilder::new()
        }
        .build();

        assert_eq!(
            names(&TestFileDiscovery.discover(&ctx)),
            vec!["src/parser.h"]
        );
    }

    #[test]
    fn test_file_discovery_pairs_every_supported_naming_convention() {
        let ctx = CtxBuilder {
            changed: vec!["src/auth.py", "web/handler.go", "ui/widget.ts"],
            candidates: vec![
                "tests/test_auth.py",
                "web/handler_test.go",
                "ui/widget.test.ts",
                "ui/widget.spec.ts",
                "ui/widget-spec.ts",
            ],
            ..CtxBuilder::new()
        }
        .build();

        assert_eq!(
            names(&TestFileDiscovery.discover(&ctx)),
            vec![
                "tests/test_auth.py",
                "ui/widget-spec.ts",
                "ui/widget.spec.ts",
                "ui/widget.test.ts",
                "web/handler_test.go",
            ]
        );
    }

    #[test]
    fn test_file_discovery_does_not_pair_on_a_prefix_match() {
        let ctx = CtxBuilder {
            changed: vec!["src/authenticate.py"],
            candidates: vec!["tests/test_auth.py", "tests/test_authenticate.py"],
            ..CtxBuilder::new()
        }
        .build();
        assert_eq!(
            names(&TestFileDiscovery.discover(&ctx)),
            vec!["tests/test_authenticate.py"]
        );
    }

    #[test]
    fn test_file_discovery_skips_changed_test_files_and_never_returns_a_changed_file() {
        // A changed `test_auth.py` must not drag in `auth.py` via this
        // strategy, and a candidate that is itself changed is never returned.
        let ctx = CtxBuilder {
            changed: vec!["tests/test_auth.py", "src/auth.py"],
            candidates: vec!["tests/test_auth.py", "src/auth.py", "tests/test_other.py"],
            ..CtxBuilder::new()
        }
        .build();
        let found = names(&TestFileDiscovery.discover(&ctx));
        assert!(!found.contains(&"src/auth.py".to_string()));
        assert!(!found.contains(&"tests/test_auth.py".to_string()));
    }

    #[test]
    fn rare_identifier_expansion_keeps_rare_terms_and_drops_common_ones() {
        let threshold = crate::config::limits::LIMITS.rare_identifier_threshold;
        let mut docs: Vec<(&'static str, &'static str)> = vec![
            ("rare_a.py", "unique_marker"),
            ("rare_b.py", "unique_marker"),
        ];
        // Push `common_marker` past the rarity threshold so it stops expanding.
        let common: [&'static str; 6] = ["c0.py", "c1.py", "c2.py", "c3.py", "c4.py", "c5.py"];
        for p in common.iter().take(threshold + 2) {
            docs.push((p, "common_marker"));
        }

        let ctx = CtxBuilder {
            concepts: vec!["unique_marker", "common_marker"],
            docs,
            ..CtxBuilder::new()
        }
        .build();

        let found = names(&expand_by_rare_identifiers(&ctx));
        assert!(
            found.contains(&"rare_a.py".to_string()),
            "rare term did not expand: {found:?}"
        );
        assert!(
            found.contains(&"rare_b.py".to_string()),
            "rare term did not expand: {found:?}"
        );
        assert!(
            !found.iter().any(|f| f.starts_with("c")),
            "a term appearing in more than {threshold} files still expanded: {found:?}"
        );
    }

    #[test]
    fn rare_identifier_expansion_is_empty_without_concepts() {
        let ctx = CtxBuilder {
            docs: vec![("a.py", "anything")],
            ..CtxBuilder::new()
        }
        .build();
        assert!(expand_by_rare_identifiers(&ctx).is_empty());
    }

    /// IDF is what makes a rare query term outrank a corpus-wide one. Negate or
    /// flatten it and BM25 silently returns the most common file instead.
    #[test]
    fn bm25_ranks_a_rare_query_term_above_a_ubiquitous_one() {
        let ctx = CtxBuilder {
            diff_text: "+ use rare_needle; use ubiquitous_helper;".into(),
            docs: vec![
                ("has_rare.py", "rare_needle body body"),
                ("common_1.py", "ubiquitous_helper body body"),
                ("common_2.py", "ubiquitous_helper body body"),
                ("common_3.py", "ubiquitous_helper body body"),
                ("common_4.py", "ubiquitous_helper body body"),
                ("common_5.py", "ubiquitous_helper body body"),
            ],
            ..CtxBuilder::new()
        }
        .build();

        let ranked = BM25Discovery::new(6).discover(&ctx);
        assert!(!ranked.is_empty(), "BM25 returned nothing");
        assert_eq!(
            names(&ranked[..1]),
            vec!["has_rare.py"],
            "the rare term did not win: {:?}",
            names(&ranked)
        );
    }

    #[test]
    fn bm25_returns_nothing_when_no_document_contains_a_query_term() {
        let ctx = CtxBuilder {
            diff_text: "+ absent_symbol_xyz".into(),
            docs: vec![("a.py", "unrelated content here")],
            ..CtxBuilder::new()
        }
        .build();
        assert!(BM25Discovery::new(5).discover(&ctx).is_empty());
    }

    #[test]
    fn bm25_returns_nothing_on_an_empty_query_or_an_empty_corpus() {
        let empty_query = CtxBuilder {
            docs: vec![("a.py", "content")],
            ..CtxBuilder::new()
        }
        .build();
        assert!(BM25Discovery::new(5).discover(&empty_query).is_empty());

        let empty_corpus = CtxBuilder {
            diff_text: "+ some_symbol".into(),
            ..CtxBuilder::new()
        }
        .build();
        assert!(BM25Discovery::new(5).discover(&empty_corpus).is_empty());
    }

    #[test]
    fn bm25_honours_top_k() {
        let ctx = CtxBuilder {
            diff_text: "+ shared_term".into(),
            docs: vec![
                ("a.py", "shared_term shared_term a"),
                ("b.py", "shared_term b b b"),
                ("c.py", "shared_term c c c c"),
            ],
            ..CtxBuilder::new()
        }
        .build();
        assert_eq!(BM25Discovery::new(2).discover(&ctx).len(), 2);
    }

    /// The ensemble is the only caller in production, and it is what hides a
    /// dead channel: if one strategy stops returning anything the others still
    /// produce results, so recall drops with no error anywhere.
    #[test]
    fn ensemble_deduplicates_across_strategies_and_preserves_first_hit_order() {
        struct Fixed(&'static str, Vec<&'static str>);
        impl DiscoveryStrategy for Fixed {
            fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
                self.1.iter().map(|p| ctx.root_dir.join(p)).collect()
            }
            fn name(&self) -> &'static str {
                self.0
            }
        }

        let ctx = CtxBuilder::new().build();
        let ensemble = EnsembleDiscovery::new(vec![
            Box::new(Fixed("first", vec!["a.py", "b.py"])),
            Box::new(Fixed("second", vec!["b.py", "c.py"])),
            Box::new(Fixed("third", vec![])),
        ]);
        let found = ensemble.discover(&ctx);
        assert_eq!(
            found
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["a.py", "b.py", "c.py"]
        );
    }

    /// The attribution the ensemble used to discard (#130). `b.py` is found by
    /// both strategies and must be credited to the one that ran first — the
    /// question it answers is "could anything surface this", not "which one
    /// deserves it".
    #[test]
    fn the_ensemble_records_which_strategy_first_surfaced_each_path() {
        struct Fixed(&'static str, Vec<&'static str>);
        impl DiscoveryStrategy for Fixed {
            fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
                self.1.iter().map(|p| ctx.root_dir.join(p)).collect()
            }
            fn name(&self) -> &'static str {
                self.0
            }
        }

        let ctx = CtxBuilder::new().build();
        let ensemble = EnsembleDiscovery::new(vec![
            Box::new(Fixed("structural", vec!["a.py", "b.py"])),
            Box::new(Fixed("lexical", vec!["b.py", "c.py"])),
        ]);
        let (paths, attribution) = ensemble.discover_attributed(&ctx);

        assert_eq!(paths.len(), 3, "dedup must still collapse the shared path");
        let by_name: Vec<(String, &str)> = attribution
            .iter()
            .map(|(p, s)| (p.file_name().unwrap().to_string_lossy().into_owned(), *s))
            .collect();
        assert_eq!(
            by_name,
            vec![
                ("a.py".to_string(), "structural"),
                ("b.py".to_string(), "structural"),
                ("c.py".to_string(), "lexical"),
            ]
        );
    }

    /// A single strategy needs no bookkeeping: everything it returns, it found.
    #[test]
    fn a_lone_strategy_attributes_everything_to_itself() {
        struct Fixed;
        impl DiscoveryStrategy for Fixed {
            fn discover(&self, ctx: &DiscoveryContext) -> Vec<PathBuf> {
                vec![ctx.root_dir.join("only.py")]
            }
            fn name(&self) -> &'static str {
                "solo"
            }
        }
        let ctx = CtxBuilder::new().build();
        let (paths, attribution) = Fixed.discover_attributed(&ctx);
        assert_eq!(paths.len(), 1);
        assert_eq!(attribution[0].1, "solo");
    }

    #[test]
    fn default_ensemble_wires_three_channels() {
        // A channel silently disappearing from the default wiring is exactly
        // the regression the ensemble's own output cannot reveal.
        assert_eq!(EnsembleDiscovery::default_ensemble().strategies.len(), 3);
    }
}
