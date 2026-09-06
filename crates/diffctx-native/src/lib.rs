//! Implementation crate for the `diffctx` CLI and Python extension. The `pub`
//! modules below are the ones the in-repo eval harness and the integration
//! tests import by path; everything else is `pub(crate)` so rustc's dead_code
//! lint stays meaningful. None of it is a stable library API — internals may
//! change or disappear between releases without a semver signal.

// Input: git plumbing and path handling.
pub mod git;
mod paths;

// Change set: what the diff touched, cut into fragments.
pub(crate) mod candidate_files;
pub(crate) mod excerpt;
pub(crate) mod fragmentation;
pub(crate) mod languages;
pub(crate) mod parsers;
pub(crate) mod signatures;
pub(crate) mod testfiles;

// Graph: relationships between fragments.
pub(crate) mod edges;
pub(crate) mod graph;
pub(crate) mod provenance;

// Ranking: relevance of each candidate to the change.
pub(crate) mod core;
pub(crate) mod discovery;
pub(crate) mod filtering;
pub(crate) mod ppr;
pub(crate) mod scoring;
pub(crate) mod stopwords;
pub(crate) mod token_corpus;
pub mod tokenizer;

// Selection: the budgeted greedy and its repairs.
pub(crate) mod interval;
pub(crate) mod postpass;
pub(crate) mod select;
pub(crate) mod utility;

// Output.
pub(crate) mod locate;
pub mod render;

// Orchestration and shared configuration.
pub mod config;
pub mod deadline;
pub mod memory_pipeline;
pub mod mode;
pub(crate) mod peak_rss;
pub mod pipeline;
#[cfg(test)]
pub(crate) mod test_rng;
pub(crate) mod types;

// `diffctx graph`: a side feature reachable only through the Python bindings.
#[cfg(feature = "python")]
pub(crate) mod analytics;
#[cfg(feature = "python")]
pub(crate) mod graph_export;
#[cfg(feature = "python")]
pub(crate) mod project_graph;

#[cfg(feature = "python")]
pub mod pybridge;
