//! Implementation crate for the `diffctx` CLI and Python extension. Every
//! module is public for those two consumers and the in-repo eval harness —
//! none of it is a stable library API, and internals may change or disappear
//! between releases without a semver signal.
pub mod analytics;
pub mod candidate_files;
pub mod config;
pub mod core;
pub mod deadline;
pub mod discovery;
pub mod edges;
pub mod excerpt;
pub mod filtering;
pub mod fragmentation;
pub mod git;
pub mod graph;
pub mod graph_export;
pub mod interval;
pub mod languages;
pub mod locate;
pub mod memory_pipeline;
pub mod mode;
pub mod parsers;
mod paths;
pub mod peak_rss;
pub mod pipeline;
pub mod postpass;
pub mod ppr;
pub mod project_graph;
pub mod provenance;
pub mod render;
pub mod scoring;
pub mod select;
pub mod signatures;
pub mod stopwords;
pub mod testfiles;
pub mod token_corpus;
pub mod tokenizer;
pub mod types;
pub mod utility;

#[cfg(feature = "python")]
pub mod pybridge;
