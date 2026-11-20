//! The facts layer of the 2.0 architecture: what an [`Extractor`] says about a
//! blob ([`BlobFacts`]), the cache keyed by blob ([`FactsCache`]), the facts of a
//! file of a snapshot ([`facts_of`]) and what a range did to the definitions
//! ([`semantic_changes`]). It reads a repository only through `diffctx_snapshot::snapshot`
//! (`tests/facts_isolation.rs` holds it). `java` and `kotlin` are the one syntax
//! walk the Kotlin, Java and Swift extractors translate into facts, and the
//! fragmenter shares; they are held to the layering by no more than that.
//!
//! An empty value is never how a failed read or a lost fact looks here: the
//! methods that make one (`Result::ok`, `unwrap_or_default`) and a direct read of
//! the disk or of a child process are denied in this module. A justified use
//! carries `#[allow(clippy::disallowed_methods, reason = "...")]`.

#![deny(clippy::disallowed_methods)]

pub mod config;
pub mod java;
pub mod kotlin;
pub mod shell;

mod blob;
mod cache;
mod changes;
mod extractor;
mod file;
pub mod framework;
mod grammar;
mod hash;
mod ids;
pub mod lang;
mod language;
mod lexical;
mod model;
mod module;
mod reflect;

pub use blob::blob_oid;
pub use cache::{
    CacheKey, CacheStats, DEFAULT_CAP_BYTES, FINGERPRINT, FactsCache, SCHEMA, default_dir,
};
pub use changes::{DefChange, Delta, Origin, SemanticChangeSet, SemanticDef, semantic_changes};
pub(crate) use changes::{holds_data, members_of};
pub use extractor::{Extractor, Registry, expired};
pub use file::facts_of;
pub use hash::{Hasher, hash_text, hash_tokens};
pub use ids::{ModulePath, Name, ProjectId, Span, TypeName};
pub use language::Language;
pub use model::{
    BlobFacts, ComputedImport, ConfigKey, ConfigSource, Def, DefKey, DefKind, Disambig, EntryKind,
    EntryPoint, Export, Fact, FileFacts, Framework, HeldAs, HeldString, Import, ImportKind,
    Initializer, Lexical, Local, LocalKey, LocalKind, Namespace, PathMention, Receiver, Ref, Scope,
    Star, UseKind, Visibility,
};

/// The facts an extractor states about a file; `None` for a language none reads.
pub(crate) fn local_facts(path: &str, text: &str) -> Option<BlobFacts> {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    let name = std::path::Path::new(path).file_name()?.to_str()?;
    let Ok(name) = diffctx_snapshot::snapshot::RepoPath::new(name) else {
        return None;
    };
    let language = Language::detect(&name, text.as_bytes())?;
    let extractor = REGISTRY.get_or_init(lang::builtin).get(language)?;
    Some(extractor.extract(
        text.as_bytes(),
        &diffctx_snapshot::snapshot::RunBudget::default(),
    ))
}
