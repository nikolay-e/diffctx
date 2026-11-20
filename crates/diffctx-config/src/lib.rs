//! The tunable parameters of diffctx as data: weights, limits, budgets, run modes, signature tables and
//! stop words, with the table of environment overrides. Reads no repository, carries no grammar and
//! no Python bindings. Not a stable library API.
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]

pub mod config;
pub mod mode;
pub mod signatures;
pub mod stopwords;
