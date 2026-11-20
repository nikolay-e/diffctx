//! What every layer of diffctx builds on, with no grammar, no Python bindings and
//! nothing above it. Not a stable library API.
#![deny(clippy::disallowed_methods)]

pub mod canon;
pub mod changed_lines;
pub mod container;
pub mod excerpt;
pub mod fit;
pub mod interval;
pub mod languages;
pub mod peak_rss;
pub mod profile;
pub mod sanitize;
pub mod testfiles;
pub mod tokenizer;
pub mod types;
