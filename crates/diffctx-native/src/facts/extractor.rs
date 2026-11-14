use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::facts::lang;
use crate::facts::language::Language;
use crate::facts::model::BlobFacts;
use crate::snapshot::RunBudget;

/// One language's reader of a blob. Pure: the same bytes give the same facts,
/// which is what lets the cache key on the blob, the build's fingerprint and the
/// grammar alone; a change to what an extractor emits is a change to the code the
/// fingerprint digests.
pub trait Extractor: Send + Sync {
    fn language(&self) -> Language;

    /// The digest of the grammar the extractor parses with, so a grammar update
    /// is a cache miss. The default reads it off the language's tree-sitter
    /// grammars; an extractor that parses otherwise returns its own.
    fn grammar(&self) -> u64 {
        lang::grammar_digest(self.language())
    }

    /// A parse with errors yields `Loss::ParseError` (see [`BlobFacts::parse_error`])
    /// and the facts that could be read, never an empty answer. Every input-sized
    /// loop polls [`expired`] and, on expiry, returns what it has with
    /// [`BlobFacts::deadline`]. Facts come back in source order
    /// ([`BlobFacts::in_source_order`]).
    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts;
}

/// Whether the run's deadline passed or it was cancelled.
pub fn expired(budget: &RunBudget) -> bool {
    budget
        .cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
        || budget.deadline.is_some_and(|at| Instant::now() >= at)
}

/// The extractors a cache serves, one per language.
#[derive(Clone, Default)]
pub struct Registry {
    by_language: BTreeMap<Language, Arc<dyn Extractor>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `extractor` for its language, replacing an earlier one.
    pub fn register(&mut self, extractor: Arc<dyn Extractor>) {
        self.by_language.insert(extractor.language(), extractor);
    }

    #[must_use]
    pub fn with(mut self, extractor: impl Extractor + 'static) -> Self {
        self.register(Arc::new(extractor));
        self
    }

    pub fn get(&self, language: Language) -> Option<&Arc<dyn Extractor>> {
        self.by_language.get(&language)
    }

    pub fn languages(&self) -> impl Iterator<Item = Language> + '_ {
        self.by_language.keys().copied()
    }
}
