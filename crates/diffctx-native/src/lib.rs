//! Implementation crate for the `diffctx` CLI and Python extension. The `pub`
//! modules below are the ones the in-repo eval harness and the integration
//! tests import by path; everything else is `pub(crate)` so rustc's dead_code
//! lint stays meaningful. None of it is a stable library API — internals may
//! change or disappear between releases without a semver signal.

// Input: git plumbing and path handling.
pub mod in_memory_harness;

// Change set: what the diff touched, cut into fragments.
pub(crate) mod decide;
pub(crate) mod fold;
pub(crate) mod fragmentation;
pub(crate) mod tokens;

// Graph: relationships between fragments.
pub(crate) mod edges;
pub(crate) mod graph;
pub(crate) mod keytable;
pub(crate) mod provenance;
pub mod resolve;
pub use tokens::occurrence;

// Ranking: relevance of each candidate to the change.
pub(crate) mod core;
pub(crate) mod discovery;
pub(crate) mod filtering;
pub(crate) mod ppr;
pub(crate) mod scoring;
pub(crate) mod token_corpus;

// Selection: the budgeted greedy and its repairs.
pub(crate) mod postpass;
pub(crate) mod select;
pub(crate) mod utility;

// Output.
pub mod answer;
pub mod artifact;
pub mod delivery;
pub mod hook;
pub mod impact;
pub mod locate;
pub mod render;

// Orchestration and shared configuration.
pub mod effective_config;
pub mod pipeline;
pub mod run_provenance;
#[cfg(test)]
pub(crate) mod test_rng;
mod vocabulary;
pub use vocabulary::{RelationKind, is_precise};

// `diffctx graph`: a side feature reachable only through the Python bindings.
#[cfg(feature = "python")]
pub(crate) mod analytics;
#[cfg(feature = "python")]
pub(crate) mod graph_export;
#[cfg(feature = "python")]
pub(crate) mod project_graph;

#[cfg(feature = "python")]
pub mod pybridge;
