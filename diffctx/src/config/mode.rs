use crate::config::env_overrides::read_env_usize;

pub struct ModeConfig {
    pub bm25_top_k_primary: usize,
    pub bm25_top_k_off: usize,
    pub ego_depth_default: usize,
    pub ego_depth_extended: usize,
}

impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            bm25_top_k_primary: 1,
            bm25_top_k_off: 0,
            ego_depth_default: 1,
            ego_depth_extended: 2,
        }
    }
}

// Returns a fresh `ModeConfig` snapshot, reading every env override on
// each call. Symmetric with `selection()`/`rescue()` in `config/selection.rs`
// — needed because in-process reuse loops (e.g. `pool_eval_all_cells` for
// a depth or budget sweep) mutate the env per cell, and a `Lazy` static
// would freeze the values at first access.
//
// `DIFFCTX_OP_GRAPH_DEPTH` overrides the graph traversal radius used by
// any scoring mode that walks the typed dependency graph (currently EGO;
// other modes that consume a depth parameter will read the same knob).
// The framework abstracts scoring as a pluggable signal, so the depth
// control is named after the underlying graph operation, not the mode.
//
// Used by the L sweep to run depth in {0, 1, 2, 3, 4} from the same
// binary without rebuilding. At depth 0 the EGO instantiation reduces
// to seed-only relevance (rel_scores carries cores at score 1.0, every
// non-core fragment is filtered before selection); the post-passes
// (coherence + rescue + changed-files) still run, so depth=0 measures
// "framework without graph propagation in scoring", not "trivial diff
// baseline".
pub fn mode() -> ModeConfig {
    ModeConfig {
        ego_depth_extended: read_env_usize("DIFFCTX_OP_GRAPH_DEPTH", 2),
        ..ModeConfig::default()
    }
}
