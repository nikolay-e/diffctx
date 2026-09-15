//! One resolved, immutable view of every parameter that shapes the semantic
//! artifact: defaults, the CLI/API arguments and the environment overrides,
//! read once at the start of a run and hashed into provenance.
//!
//! Until this existed the parameter surface was spread over `Lazy` statics
//! (read at first access), per-call accessors (`selection()`, `mode()`), and
//! ad-hoc `std::env::var` toggles inside the phases, so two runs whose
//! outputs differed could not say what differed between them. The values
//! recorded here are read through the same accessors the phases use — the
//! record is what ran, not a second reading of the environment.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use crate::config::filtering::FILTERING;
use crate::config::limits::{PPR, UTILITY};
use crate::config::needs::NEEDS;
use crate::config::scoring::{EGO, PIT, RRF};
use crate::mode::{ObjectiveMode, PipelineConfig, ScoringMode};
use crate::tokenizer::{TokenCounter, TokenizerId};

pub const SCHEMA: &str = "diffctx.effective_config.v1";

/// Bumped whenever a calibrated constant in `config/` (tau, cbf, the mode
/// tables, the per-hop decay) changes value: a run under one profile is not
/// comparable with a run under another even at the same config hash, because
/// the hash records what the environment overrode, not the defaults.
pub const SCORING_PROFILE_VERSION: &str = "v5-2026-08-19";

/// Bumped whenever an entry of `config/weights.rs` or `config/edge_weights.rs`
/// changes: the manual edge weights are frozen model parameters for v3.
pub const EDGE_WEIGHT_PROFILE_VERSION: &str = "v2-2026-09-16";

/// Bumped whenever fragmentation changes what a file is cut into (a grammar
/// upgrade, a new `definition_types` entry, a chunking rule): a parse cache
/// keyed by content must not serve fragments an older parser produced.
pub const PARSER_PROFILE_VERSION: &str = "v1-2026-09-02";

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct SelectionParams {
    pub core_budget_fraction: f64,
    pub r_cap_min: f64,
    pub per_file_budget_fraction: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct RescueParams {
    pub budget_fraction: f64,
    pub min_score_percentile: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct BoltzmannParams {
    pub calibration_tolerance: f64,
    pub bisect_iters: u32,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct EgoParams {
    pub identifier_overlap_epsilon: f64,
    pub per_hop_decay: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct PitParams {
    pub blend: f64,
    pub agreement_bonus: f64,
    pub agreement_top_k: usize,
    pub shape: String,
    pub transform: String,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct PprParams {
    pub alpha: f64,
    pub forward_blend: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct UtilityParams {
    pub eta: f64,
    pub structural_bonus_weight: f64,
    pub r_cap_sigma: f64,
    pub proximity_decay: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct FilteringParams {
    pub proximity_half_decay: f64,
    pub definition_proximity_half_decay: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct NeedsParams {
    pub min_rel_for_bonus: f64,
    pub relatedness_bonus: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct TokenizerParams {
    pub id: TokenizerId,
    pub safety_factor: f64,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct Profiles {
    pub scoring: &'static str,
    pub edge_weights: &'static str,
    pub parser: &'static str,
}

#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
pub struct EffectiveConfigV1 {
    pub schema: &'static str,
    pub scoring: &'static str,
    pub objective: &'static str,
    /// The damping the run used: the CLI/API argument, which outranks
    /// `DIFFCTX_OP_PPR_ALPHA` (recorded under `ppr.alpha`).
    pub alpha: f64,
    pub graph_depth: usize,
    pub bm25_discovery_top_k: usize,
    pub file_admission: bool,
    pub commit_signal: bool,
    /// Every cap the run is held to. A cap that binds changes the artifact,
    /// so the caps are part of the configuration and of its hash.
    pub resources: crate::resource::ResourceBudget,
    pub selection: SelectionParams,
    pub rescue: RescueParams,
    pub boltzmann: BoltzmannParams,
    pub ego: EgoParams,
    pub rrf_k: f64,
    pub pit: PitParams,
    pub ppr: PprParams,
    pub utility: UtilityParams,
    pub filtering: FilteringParams,
    pub needs: NeedsParams,
    pub tokenizer: TokenizerParams,
    pub profiles: Profiles,
    /// The `DIFFCTX_*` names set in the environment that this config read,
    /// with their raw values. Excluded from the hash: two spellings of one
    /// value ("0.4" and "0.40") are one configuration.
    pub overrides: BTreeMap<String, String>,
}

/// Names whose value changes the semantic artifact. Every one of them is
/// resolved into a field above, so the hash covers them.
pub const SEMANTIC_ENV: &[&str] = &[
    "DIFFCTX_BM25_DISCOVERY_TOP_K",
    "DIFFCTX_EGO_LEXICAL_EPS",
    "DIFFCTX_EGO_PER_HOP_DECAY",
    "DIFFCTX_FILE_ADMISSION",
    "DIFFCTX_MAX_CANDIDATE_FILES",
    "DIFFCTX_MAX_EDGE_CONTRIBUTIONS",
    "DIFFCTX_MAX_EDGES_PER_NODE",
    "DIFFCTX_MAX_FRAGMENTS",
    "DIFFCTX_MAX_NEEDS",
    "DIFFCTX_MAX_SOURCE_BYTES",
    "DIFFCTX_MIN_REL_FOR_BONUS",
    "DIFFCTX_NO_COMMIT_SIGNAL",
    "DIFFCTX_OBJECTIVE",
    "DIFFCTX_OP_BOLTZMANN_BISECT_ITERS",
    "DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE",
    "DIFFCTX_OP_FILTERING_DEFINITION_PROXIMITY_HALF_DECAY",
    "DIFFCTX_OP_FILTERING_PROXIMITY_HALF_DECAY",
    "DIFFCTX_OP_GRAPH_DEPTH",
    "DIFFCTX_OP_PPR_ALPHA",
    "DIFFCTX_OP_PPR_FORWARD_BLEND",
    "DIFFCTX_OP_RESCUE_BUDGET_FRACTION",
    "DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE",
    "DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION",
    "DIFFCTX_OP_SELECTION_PER_FILE_BUDGET_FRACTION",
    "DIFFCTX_OP_SELECTION_R_CAP_MIN",
    "DIFFCTX_OP_UTILITY_ETA",
    "DIFFCTX_OP_UTILITY_PROXIMITY_DECAY",
    "DIFFCTX_OP_UTILITY_R_CAP_SIGMA",
    "DIFFCTX_OP_UTILITY_STRUCTURAL_BONUS_WEIGHT",
    "DIFFCTX_PIT_AGREEMENT_BONUS",
    "DIFFCTX_PIT_AGREEMENT_TOP_K",
    "DIFFCTX_PIT_BLEND",
    "DIFFCTX_PIT_SHAPE",
    "DIFFCTX_PIT_TRANSFORM",
    "DIFFCTX_RELATEDNESS_BONUS",
    "DIFFCTX_RRF_K",
    "DIFFCTX_TOKEN_SAFETY_FACTOR",
];

/// Names that are legitimately set around a run without changing what it
/// selects: telemetry sinks, caches, harness knobs, the Python surface's own
/// settings. Strict mode lets these through.
pub const NON_SEMANTIC_ENV: &[&str] = &[
    "DIFFCTX_ALLOWED_PATHS",
    "DIFFCTX_BENCH_TIMEOUT_SEC",
    "DIFFCTX_BUILD_SHA",
    "DIFFCTX_CONFIG_DIR",
    "DIFFCTX_DIR_IGNORE",
    "DIFFCTX_DIR_WHITELIST",
    "DIFFCTX_DUMP_DIR",
    "DIFFCTX_DUMP_SCORES",
    "DIFFCTX_EVAL_STRICT",
    "DIFFCTX_IGNORE_RELPATH",
    "DIFFCTX_MCP_LEGACY_TOOLS",
    "DIFFCTX_PROBE_MODE",
    "DIFFCTX_PROBE_TAU",
    "DIFFCTX_PROVENANCE",
    "DIFFCTX_PROVENANCE_DUMP",
    "DIFFCTX_RANDOM_BASELINE_SEED",
    "DIFFCTX_SCORING",
    "DIFFCTX_TOKEN_CACHE_DIR",
    "DIFFCTX_TOKEN_CACHE_MAX_BYTES",
    "DIFFCTX_TRACE_BUILDERS",
    "DIFFCTX_YAML_CASES_LIMIT",
    "DIFFCTX_YAML_IGNORE_BASELINE",
    "DIFFCTX_YAML_MIN_SCORE",
];

impl EffectiveConfigV1 {
    /// Reads the environment through the same accessors the phases use, so a
    /// value recorded here is the value that ran.
    pub fn resolve(config: &PipelineConfig, timeout_secs: u64) -> Self {
        let selection = crate::config::selection::selection();
        let rescue = crate::config::selection::rescue();
        let boltzmann = crate::config::selection::boltzmann();
        let overrides: BTreeMap<String, String> = std::env::vars()
            .filter(|(name, _)| SEMANTIC_ENV.contains(&name.as_str()))
            .collect();
        Self {
            schema: SCHEMA,
            scoring: scoring_name(config.scoring),
            objective: match config.objective {
                ObjectiveMode::Submodular => "submodular",
                ObjectiveMode::BoltzmannModular => "boltzmann_modular",
            },
            alpha: config.ppr_alpha,
            graph_depth: config.ego_depth,
            bm25_discovery_top_k: config.bm25_top_k,
            file_admission: crate::scoring::file_admission_enabled(),
            commit_signal: std::env::var("DIFFCTX_NO_COMMIT_SIGNAL").as_deref() != Ok("1"),
            resources: crate::resource::ResourceBudget::resolve(timeout_secs),
            selection: SelectionParams {
                core_budget_fraction: selection.core_budget_fraction,
                r_cap_min: selection.r_cap_min,
                per_file_budget_fraction: selection.per_file_budget_fraction,
            },
            rescue: RescueParams {
                budget_fraction: rescue.budget_fraction,
                min_score_percentile: rescue.min_score_percentile,
            },
            boltzmann: BoltzmannParams {
                calibration_tolerance: boltzmann.calibration_tolerance,
                bisect_iters: boltzmann.bisect_iters,
            },
            ego: EgoParams {
                identifier_overlap_epsilon: EGO.identifier_overlap_epsilon,
                per_hop_decay: EGO.per_hop_decay,
            },
            rrf_k: RRF.k,
            pit: PitParams {
                blend: PIT.blend,
                agreement_bonus: PIT.agreement_bonus,
                agreement_top_k: PIT.agreement_top_k,
                shape: env_or("DIFFCTX_PIT_SHAPE", "quantile"),
                transform: env_or("DIFFCTX_PIT_TRANSFORM", "percentile"),
            },
            ppr: PprParams {
                alpha: PPR.alpha,
                forward_blend: PPR.forward_blend,
            },
            utility: UtilityParams {
                eta: UTILITY.eta,
                structural_bonus_weight: UTILITY.structural_bonus_weight,
                r_cap_sigma: UTILITY.r_cap_sigma,
                proximity_decay: UTILITY.proximity_decay,
            },
            filtering: FilteringParams {
                proximity_half_decay: FILTERING.proximity_half_decay,
                definition_proximity_half_decay: FILTERING.definition_proximity_half_decay,
            },
            needs: NeedsParams {
                min_rel_for_bonus: NEEDS.min_rel_for_bonus,
                relatedness_bonus: NEEDS.relatedness_bonus,
            },
            tokenizer: TokenizerParams {
                id: crate::tokenizer::ACCOUNTING.id(),
                safety_factor: crate::tokenizer::safety_factor(),
            },
            profiles: Profiles {
                scoring: SCORING_PROFILE_VERSION,
                edge_weights: EDGE_WEIGHT_PROFILE_VERSION,
                parser: PARSER_PROFILE_VERSION,
            },
            overrides,
        }
    }

    /// 16 hex digits of FNV-1a over the canonical JSON of everything except
    /// `overrides`. Stable across processes and platforms — serde_json orders
    /// map keys and prints floats deterministically — so the same effective
    /// configuration hashes the same on every machine.
    pub fn hash(&self) -> String {
        let mut canonical = self.clone();
        canonical.overrides.clear();
        let json = serde_json::to_string(&canonical).expect("effective config serializes");
        format!("{:016x}", fnv1a64(json.as_bytes()))
    }
}

fn scoring_name(mode: ScoringMode) -> &'static str {
    match mode {
        ScoringMode::Ppr => "ppr",
        ScoringMode::Ego => "ego",
        ScoringMode::Bm25 => "bm25",
        ScoringMode::Rrf => "rrf",
        ScoringMode::Pit => "pit",
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `DIFFCTX_EVAL_STRICT=1`: an evaluation run must not be shaped by a knob the
/// record does not know about. Every `DIFFCTX_*` name in the environment has
/// to be either a semantic parameter (recorded and hashed) or a declared
/// non-semantic one; anything else is an undeclared override and the run
/// refuses to start rather than produce a number nobody can reproduce.
pub fn enforce_strict_env() -> anyhow::Result<()> {
    if std::env::var("DIFFCTX_EVAL_STRICT").as_deref() != Ok("1") {
        return Ok(());
    }
    let unknown = undeclared_names(std::env::vars().map(|(name, _)| name));
    if unknown.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "DIFFCTX_EVAL_STRICT=1: undeclared DIFFCTX_* overrides in the environment: {}. \
         A strict run records every parameter it consumes; unset these or declare them in \
         effective_config.rs",
        unknown.join(", ")
    )
}

pub fn undeclared_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let declared: BTreeSet<&str> = SEMANTIC_ENV
        .iter()
        .chain(NON_SEMANTIC_ENV.iter())
        .copied()
        .collect();
    let mut unknown: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with("DIFFCTX_") && !declared.contains(n.as_str()))
        .collect();
    unknown.sort();
    unknown.dedup();
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EffectiveConfigV1 {
        EffectiveConfigV1::resolve(&PipelineConfig::from_mode(ScoringMode::Ego), 300)
    }

    #[test]
    fn the_hash_is_stable_and_ignores_override_spelling() {
        let a = config();
        let mut b = config();
        b.overrides
            .insert("DIFFCTX_OP_GRAPH_DEPTH".into(), "2".into());
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 16);
    }

    #[test]
    fn a_different_parameter_hashes_differently() {
        let a = config();
        let mut b = config();
        b.graph_depth += 1;
        assert_ne!(a.hash(), b.hash());
        let mut c = config();
        c.tokenizer.safety_factor = 1.3;
        assert_ne!(a.hash(), c.hash());
    }

    #[test]
    fn strict_mode_names_exactly_the_undeclared_overrides() {
        let unknown = undeclared_names(
            [
                "DIFFCTX_OP_GRAPH_DEPTH",
                "DIFFCTX_PROVENANCE_DUMP",
                "DIFFCTX_SECRET_KNOB",
                "PATH",
                "DIFFCTX_SECRET_KNOB",
            ]
            .map(String::from),
        );
        assert_eq!(unknown, vec!["DIFFCTX_SECRET_KNOB".to_string()]);
    }

    /// Every `DIFFCTX_*` literal the crate reads must be classified, or a new
    /// knob becomes a hidden parameter the moment it is added.
    #[test]
    fn every_env_name_the_crate_reads_is_declared() {
        let re = regex::Regex::new(r#""(DIFFCTX_[A-Z0-9_]+)""#).unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect(&root, &mut files);
        let mut read: BTreeSet<String> = BTreeSet::new();
        for file in files {
            if file.ends_with("effective_config.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for cap in re.captures_iter(&text) {
                read.insert(cap[1].to_string());
            }
        }
        let unknown = undeclared_names(read.into_iter());
        assert!(
            unknown.is_empty(),
            "undeclared env names read by the crate: {unknown:?}"
        );
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}
