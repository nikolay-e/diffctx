//! The fragment graph of diffctx and what ranks it: files cut into fragments, the edges between them
//! (semantic, configuration, similarity, structural, history), the typed key table, the scores that rank
//! a fragment against a change, candidate discovery and the token corpus it reads. Takes what the layers
//! below it read and gives a graph, never an answer. Not a stable library API.

pub mod core;
pub mod discovery;
pub mod edges;
pub mod filtering;
pub mod fragmentation;
pub mod graph;
pub mod keytable;
pub mod ppr;
pub mod scoring;
pub mod token_corpus;
