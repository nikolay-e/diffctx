//! Implementation crate for the `diffctx` CLI and Python extension. The `pub`
//! modules below are the ones the in-repo eval harness and the integration
//! tests import by path; everything else is `pub(crate)` so rustc's dead_code
//! lint stays meaningful. None of it is a stable library API — internals may
//! change or disappear between releases without a semver signal.

// Input: git plumbing and path handling.
pub mod in_memory_harness;

// Change set: what the diff touched, cut into fragments.

// Graph: relationships between fragments.

// Ranking: relevance of each candidate to the change.

// Selection: the budgeted greedy and its repairs.

// Output.
pub mod artifact;
pub mod delivery;
pub mod hook;

// Orchestration and shared configuration.
pub mod pipeline;

// `diffctx graph`: a side feature reachable only through the Python bindings.
#[cfg(feature = "python")]
pub(crate) mod analytics;
#[cfg(feature = "python")]
pub(crate) mod graph_export;
#[cfg(feature = "python")]
pub(crate) mod project_graph;

#[cfg(feature = "python")]
pub mod pybridge;
