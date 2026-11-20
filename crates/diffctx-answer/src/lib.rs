//! What a diffctx run says: the Answer document (envelope, v2 schema, renderings), the impact and locate
//! answers, what an answer may claim about the uses of a definition and about the coverage of the search
//! (`decide`, `fold`), and what reproduces or compares a run (`effective_config`, `run_provenance`,
//! `provenance`). Not a stable library API.

pub mod answer;
pub mod decide;
pub mod effective_config;
pub mod fold;
pub mod impact;
pub mod locate;
pub mod provenance;
pub mod render;
pub mod run_provenance;
