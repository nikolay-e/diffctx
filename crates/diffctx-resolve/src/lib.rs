//! The resolvers of diffctx: a name to its definition per language, from the imports and scopes of the
//! facts layer's files, with the relation kinds and tiers they answer in. A resolver binds and never
//! gives a verdict on what a use may claim. Reads a repository only through diffctx-snapshot and
//! diffctx-facts. Not a stable library API.

pub mod resolve;
pub mod vocabulary;
